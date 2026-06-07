use sqlx::{PgPool, Row};
use sqlx::types::Json;
use uuid::Uuid;
use regex::Regex;
use serde_json::{json, Value};

#[derive(serde::Serialize)]
struct ScrapeRequest<'a> {
    url: &'a str,
    formats: Vec<&'a str>,
}

#[derive(serde::Deserialize)]
struct ScrapeResponse {
    success: Option<bool>,
    data: Option<ScrapeData>,
}

#[derive(serde::Deserialize)]
struct ScrapeData {
    markdown: Option<String>,
    metadata: Option<ScrapeMetadata>,
}

#[derive(serde::Deserialize)]
struct ScrapeMetadata {
    title: Option<String>,
}

pub async fn scrape_target_url(pool: &PgPool, target_url: &str, service_category: &str, config: &crate::MarketConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("[Crawler] Scraping target URL: {} (Category: {}) for market: {}", target_url, service_category, config.target_city);

    // Hitting local Firecrawl API instance running at http://localhost:3002
    let client = reqwest::Client::new();
    let payload = ScrapeRequest {
        url: target_url,
        formats: vec!["markdown"],
    };

    let response = client
        .post("http://localhost:3002/v1/scrape")
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Firecrawl responded with error status: {}", response.status()).into());
    }

    let result: ScrapeResponse = response.json().await?;
    
    if result.success != Some(true) {
        return Err("Firecrawl scrape request was not marked as successful".into());
    }

    let scrape_data = result.data.ok_or("Missing 'data' block in Firecrawl response")?;
    let homepage_markdown = scrape_data.markdown.unwrap_or_default();
    let metadata = scrape_data.metadata;
    
    // Determine business/legal entity name from title or fallback
    let extracted_title = metadata
        .and_then(|m| m.title)
        .unwrap_or_else(|| "Unknown Contractor".to_string());

    // Clean and normalize the name (strip title suffixes if needed)
    let cleaned_name = clean_business_name(&extracted_title);
    let cleaned_name = html_escape::decode_html_entities(&cleaned_name).into_owned();

    // Defensive Filtering: Abort if page is thin, blank, or an error page
    let title_lower = extracted_title.to_lowercase();
    if title_lower.contains("error") 
        || title_lower.contains("404") 
        || title_lower.contains("blocked")
        || title_lower.contains("access denied")
        || title_lower.contains("just a moment") // Cloudflare challenge
        || title_lower.contains("page not found")
        || cleaned_name.to_lowercase().contains("unknown")
        || homepage_markdown.trim().is_empty()
        || homepage_markdown.len() < 150 
    {
        return Err(format!("Fidelity validation failed: Scraped page is thin, blank, or an error block. (Title: '{}', Markdown Len: {})", extracted_title, homepage_markdown.len()).into());
    }

    // --- Dynamic Navigation & Recursive Service Page Ingestion ---
    let sub_links = extract_navigation_links(&homepage_markdown, target_url);
    println!("[Crawler] Discovered {} potential service sub-pages to ingest recursively.", sub_links.len());

    let mut raw_markdown = format!("--- Homepage: {} ---\n\n{}", target_url, homepage_markdown);

    // Fetch and aggregate up to 5 sub-pages to keep LLM token budget under control
    let max_sub_pages = 5;
    let mut pages_scraped = 0;
    
    for sub_url in sub_links {
        if pages_scraped >= max_sub_pages {
            break;
        }
        
        println!("[Crawler] Recursively scraping sub-page: {}", sub_url);
        let sub_payload = ScrapeRequest {
            url: &sub_url,
            formats: vec!["markdown"],
        };
        
        if let Ok(sub_resp) = client
            .post("http://localhost:3002/v1/scrape")
            .json(&sub_payload)
            .send()
            .await
        {
            if sub_resp.status().is_success() {
                if let Ok(sub_result) = sub_resp.json::<ScrapeResponse>().await {
                    if sub_result.success == Some(true) {
                        if let Some(sub_data) = sub_result.data {
                            if let Some(sub_markdown) = sub_data.markdown {
                                if !sub_markdown.trim().is_empty() {
                                    raw_markdown.push_str(&format!("\n\n--- Page: {} ---\n\n{}", sub_url, sub_markdown));
                                    pages_scraped += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    println!("[Crawler] Scraping complete. Total pages aggregated: {}", pages_scraped + 1);

    // Isolate basic NAP phone number signature via regex (run on the aggregated markdown!)
    let phone_opt = extract_phone_number(&raw_markdown);

    // Check if website_url already exists in the database for this market
    let existing = sqlx::query("SELECT id, enriched_data FROM contractors WHERE website_url = $1 AND target_city = $2")
        .bind(target_url)
        .bind(&config.target_city)
        .fetch_optional(pool)
        .await?;

    if let Some(row) = existing {
        let existing_id: String = row.get("id");
        let db_enriched_json = row.get::<Option<Json<Value>>, _>("enriched_data");
        let mut db_enriched: Value = db_enriched_json.map(|j| j.0).unwrap_or_else(|| json!({}));

        // Update enriched_data with the raw markdown and title
        if let Some(obj) = db_enriched.as_object_mut() {
            obj.insert("raw_markdown".to_string(), json!(raw_markdown));
            obj.insert("raw_scraped_title".to_string(), json!(extracted_title));
            obj.insert("scraped_markdown_len".to_string(), json!(raw_markdown.len()));
            obj.insert("verification_source".to_string(), json!("firecrawl_ingestion"));
        }
        let enriched_data_str = serde_json::to_string(&db_enriched)?;

        println!("[DB] Saving {} to Postgres...", cleaned_name);

        match sqlx::query(
            "UPDATE contractors 
             SET phone_number = $1, enriched_data = $2::jsonb, lifecycle_state = 0 
             WHERE id = $3"
        )
        .bind(&phone_opt)
        .bind(&enriched_data_str)
        .bind(&existing_id)
        .execute(pool)
        .await
        {
            Ok(_) => {
                println!("[DB] Save successful");
                println!("[Crawler] Updated existing contractor record ID {} with newly scraped markdown.", existing_id);
            }
            Err(e) => {
                println!("[DB] Save failed: {}", e);
                return Err(e.into());
            }
        }
    } else {
        // Generate a unique UUID for the new record
        let contractor_id = Uuid::new_v4().to_string();

        // Prepare metadata block for enriched_data JSON field
        let enriched_payload = json!({
            "raw_scraped_title": extracted_title,
            "scraped_markdown_len": raw_markdown.len(),
            "raw_markdown": raw_markdown,
            "verification_source": "firecrawl_ingestion"
        });

        print!(
            "[Crawler] Successfully parsed. Legal Name: '{}', Phone: {:?}",
            cleaned_name, phone_opt
        );

        // Map service category to a human-readable vertical
        let primary_vertical = match service_category {
            "plumbing" => "Plumbing Specialists",
            "electrical" => "Electrical Services",
            "pest_control" => "Pest Control & Extermination",
            "septic_system" => "On-Site Septic System Services",
            _ => "Kitchen Remodeling",
        };

        let enriched_data_str = serde_json::to_string(&enriched_payload)?;

        println!("[DB] Saving {} to Postgres...", cleaned_name);

        // Insert the entity into our contractors table sitting at lifecycle_state = 0
        match sqlx::query(
            "INSERT INTO contractors (
                id, 
                contractor_license_number, 
                legal_entity_name, 
                zip_code, 
                primary_vertical, 
                phone_number, 
                website_url, 
                lifecycle_state, 
                enriched_data,
                service_category,
                target_city
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10, $11)"
        )
        .bind(&contractor_id)
        .bind(None::<String>) // contractor_license_number to be cleaned/enriched later
        .bind(&cleaned_name)
        .bind(None::<String>) // zip_code to be enriched/extracted later
        .bind(primary_vertical) // primary_vertical mapped dynamic vertical
        .bind(&phone_opt)
        .bind(target_url)
        .bind(0) // lifecycle_state = 0 (Scraped)
        .bind(&enriched_data_str)
        .bind(service_category)
        .bind(&config.target_city)
        .execute(pool)
        .await
        {
            Ok(_) => {
                println!("[DB] Save successful");
                println!("[Crawler] Committed new contractor record successfully!");
            }
            Err(e) => {
                println!("[DB] Save failed: {}", e);
                return Err(e.into());
            }
        }
    }

    Ok(())
}

fn clean_business_name(raw_title: &str) -> String {
    // Strip common site title trailing boilerplates
    let parts: Vec<&str> = raw_title.split('|').collect();
    let first_part = parts.first().unwrap_or(&raw_title).trim();
    
    let sub_parts: Vec<&str> = first_part.split('-').collect();
    let cleaned = sub_parts.first().unwrap_or(&first_part).trim();

    if cleaned.is_empty() {
        "Unknown Contractor".to_string()
    } else {
        cleaned.to_string()
    }
}

fn extract_phone_number(text: &str) -> Option<String> {
    let phone_regex = Regex::new(r"\(?[2-9]\d{2}\)?[-.\s]?\d{3}[-.\s]?\d{4}").ok()?;
    let mut scored_numbers = Vec::new();

    for mat in phone_regex.find_iter(text) {
        let raw_phone = mat.as_str();
        let digits: String = raw_phone.chars().filter(|c| c.is_ascii_digit()).collect();
        
        if digits.len() == 10 {
            let area_code = &digits[0..3];
            let mut score = 0;
            
            if area_code == "360" {
                score += 50;
            }
            
            if area_code == "571" 
                || area_code == "800" 
                || area_code == "888" 
                || area_code == "877" 
                || area_code == "866" 
                || area_code == "855" 
                || area_code == "844" 
                || area_code == "833" 
            {
                score -= 50;
            }
            
            scored_numbers.push((score, digits));
        }
    }
    
    // Sort descending by score
    scored_numbers.sort_by(|a, b| b.0.cmp(&a.0));
    
    scored_numbers.first().map(|(_, digits)| format!("+1{}", digits))
}

fn extract_domain(url: &str) -> Option<String> {
    let cleaned = url.trim().to_lowercase();
    let without_protocol = if let Some(stripped) = cleaned.strip_prefix("https://") {
        stripped
    } else if let Some(stripped) = cleaned.strip_prefix("http://") {
        stripped
    } else {
        &cleaned
    };
    let without_www = without_protocol.strip_prefix("www.").unwrap_or(without_protocol);
    let parts: Vec<&str> = without_www.split('/').collect();
    parts.first().map(|s| s.to_string())
}

fn extract_navigation_links(homepage_markdown: &str, base_url: &str) -> Vec<String> {
    let mut links = Vec::new();
    let link_regex = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").unwrap();
    let base_domain = extract_domain(base_url).unwrap_or_default();
    
    for cap in link_regex.captures_iter(homepage_markdown) {
        let text = cap.get(1).map(|m| m.as_str().to_lowercase()).unwrap_or_default();
        let href = cap.get(2).map(|m| m.as_str().trim()).unwrap_or_default();
        
        if href.is_empty() 
            || href.starts_with('#') 
            || href.starts_with("javascript:") 
            || href.starts_with("tel:") 
            || href.starts_with("mailto:") 
        {
            continue;
        }
        
        let mut absolute_url = href.to_string();
        if href.starts_with('/') {
            let base_trimmed = base_url.trim_end_matches('/');
            absolute_url = format!("{}{}", base_trimmed, href);
        } else {
            let href_domain = extract_domain(href).unwrap_or_default();
            if href_domain != base_domain {
                continue;
            }
        }
        
        let is_service = href.contains("service")
            || href.contains("remodel")
            || href.contains("kitchen")
            || href.contains("bath")
            || href.contains("plumb")
            || href.contains("electr")
            || href.contains("hvac")
            || href.contains("pest")
            || href.contains("septic")
            || href.contains("cabinet")
            || text.contains("service")
            || text.contains("what we do")
            || text.contains("capabilities");
            
        if is_service && !links.contains(&absolute_url) {
            links.push(absolute_url);
        }
    }
    
    links
}
