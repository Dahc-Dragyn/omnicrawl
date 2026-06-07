use sqlx::{Row, PgPool};
use sqlx::types::Json;
use serde_json::{json, Value};
use strsim::jaro_winkler;

pub async fn refine_contractors(pool: &PgPool, config: &crate::MarketConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("[Refinement] Initiating Phase 3 (Deterministic Refinement Loop) for market {}...", config.target_city);

    // Load Gemini API Key for sanitization check
    let api_key = std::env::var("GEMINI_API_KEY")
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .map_err(|_| "GEMINI_API_KEY or GOOGLE_API_KEY environment variable must be set for refinement/sanitization phase")?;

    let client = reqwest::Client::new();
    let gemini_url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent?key={}",
        api_key
    );

    let rows = sqlx::query(
        "SELECT id, legal_entity_name, phone_number, website_url, contractor_license_number, enriched_data, service_category FROM contractors WHERE lifecycle_state = 0 AND target_city = $1"
    )
    .bind(&config.target_city)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("[Refinement] No records sitting at lifecycle_state = 0 found for market {}. Refinement complete.", config.target_city);
        return Ok(());
    }

    println!("[Refinement] Processing {} records for deduplication and cleaning...", rows.len());

    let existing_rows = sqlx::query(
        "SELECT id, legal_entity_name, phone_number, website_url FROM contractors WHERE lifecycle_state >= 1 AND target_city = $1"
    )
    .bind(&config.target_city)
    .fetch_all(pool)
    .await?;

    let mut processed_domains = std::collections::HashSet::new();
    let mut processed_phones = std::collections::HashSet::new();
    let mut processed_slugs = std::collections::HashSet::new();

    // Populate processed sets from existing_rows (state >= 1) to compare against
    for exist_row in &existing_rows {
        let name: String = exist_row.get("legal_entity_name");
        let phone: Option<String> = exist_row.get("phone_number");
        let website: Option<String> = exist_row.get("website_url");

        if let Some(ref w) = website {
            if let Some(dom) = extract_domain(w) {
                if !dom.is_empty() {
                    processed_domains.insert(dom);
                }
            }
        }

        if let Some(ref p) = phone {
            let normalized = p.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
            if !normalized.is_empty() && normalized != "unknown" {
                processed_phones.insert(normalized);
            }
        }

        let slug = name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "-");
        let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<&str>>().join("-");
        if !slug.is_empty() {
            processed_slugs.insert(slug);
        }
    }

    for row in rows {
        let row_id: String = row.get("id");
        let name: String = row.get("legal_entity_name");
        let phone: Option<String> = row.get("phone_number");
        let website: Option<String> = row.get("website_url");
        let contractor_license_number: Option<String> = row.get("contractor_license_number");
        let db_enriched_json = row.get::<Option<Json<Value>>, _>("enriched_data");
        let db_enriched: Value = db_enriched_json.map(|j| j.0).unwrap_or_else(|| serde_json::json!({}));
        let raw_markdown = db_enriched.get("raw_markdown").and_then(|v| v.as_str()).unwrap_or("");
        let service_category: String = row.get("service_category");

        let mut is_duplicate = false;

        // 1. Domain-based matching (shares root domain with another company in batch or DB)
        let domain_opt = website.as_ref().and_then(|w| extract_domain(w));
        if let Some(ref dom) = domain_opt {
            if !dom.is_empty() && processed_domains.contains(dom) {
                println!("[Refinement] Duplicate detected by domain! '{}' shares root domain '{}'", website.as_deref().unwrap_or(""), dom);
                is_duplicate = true;
            }
        }

        // 2. Phone number match
        if !is_duplicate {
            if let Some(ref p) = phone {
                let normalized = p.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
                if !normalized.is_empty() && normalized != "unknown" {
                    if processed_phones.contains(&normalized) {
                        println!("[Refinement] Duplicate detected by phone! '{}'", p);
                        is_duplicate = true;
                    }
                }
            }
        }

        // 3. Name slug match (exact brand name slug match)
        if !is_duplicate {
            let slug = name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "-");
            let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<&str>>().join("-");
            if !slug.is_empty() && processed_slugs.contains(&slug) {
                println!("[Refinement] Duplicate detected by slug/name! '{}'", name);
                is_duplicate = true;
            }
        }

        // 4. Jaro-Winkler name similarity with existing items
        if !is_duplicate {
            for exist_row in &existing_rows {
                let exist_name: String = exist_row.get("legal_entity_name");
                let score = jaro_winkler(&name, &exist_name);
                if score > 0.88 {
                    println!("[Refinement] Duplicate detected by name similarity! '{}' matches '{}' (JW: {:.2})", name, exist_name, score);
                    is_duplicate = true;
                    break;
                }
            }
        }

        // 5. Generic names check
        if !is_duplicate {
            let name_lower = name.to_lowercase();
            if name_lower.contains("unknown") || name_lower.contains("error") {
                println!("[Refinement] Generic placeholder record flagged as duplicate: '{}'", name);
                is_duplicate = true;
            }
        }

        if is_duplicate {
            sqlx::query("UPDATE contractors SET lifecycle_state = -1 WHERE id = $1")
                .bind(&row_id)
                .execute(pool)
                .await?;
            println!("[Refinement] Flagged duplicate ID {}", row_id);
        } else {
            // --- Geo-Verification Pass ---
            let mut geo_score = 0;
            if let Some(ref p) = phone {
                if is_local_phone(p, config) {
                    geo_score += 100;
                } else {
                    let digits: String = p.chars().filter(|c| c.is_ascii_digit()).collect();
                    let normalized = if digits.starts_with('1') && digits.len() == 11 {
                        &digits[1..]
                    } else {
                        &digits
                    };
                    if !normalized.is_empty() {
                        geo_score -= 500;
                    }
                }
            }

            let mut has_ccph = false;
            if let Some(ref lic) = contractor_license_number {
                if lic.to_uppercase().contains("CCPH") {
                    has_ccph = true;
                }
            }
            if raw_markdown.to_uppercase().contains("CCPH") {
                has_ccph = true;
            }

            let mut location_confirmed = false;
            if has_ccph {
                geo_score += 100;
                location_confirmed = true;
            } else {
                let md_upper = raw_markdown.to_uppercase();
                let city_upper = config.target_city.to_uppercase();
                let state_upper = config.target_state.to_uppercase();
                let city_state_comma = format!("{}, {}", city_upper, state_upper);
                let city_state_space = format!("{} {}", city_upper, state_upper);
                
                if md_upper.contains(&city_state_comma) 
                    || md_upper.contains(&city_state_space) 
                    || (city_upper == "VANCOUVER" && md_upper.contains("CLARK COUNTY")) 
                {
                    location_confirmed = true;
                }
            }

            if !location_confirmed {
                geo_score = 0;
            }

            if geo_score <= 0 {
                sqlx::query("UPDATE contractors SET lifecycle_state = -2 WHERE id = $1")
                    .bind(&row_id)
                    .execute(pool)
                    .await?;
                println!("[Filter] Exclusion: {} (ID: {}) failed GeoVerification. Score: {}.", name, row_id, geo_score);
            } else {
                let title = db_enriched.get("raw_scraped_title").and_then(|v| v.as_str()).unwrap_or("");
                let snippet: String = raw_markdown.chars().take(500).collect();

                println!("[Sanitizer] Running agentic sanitization check for '{}' (ID: {})...", name, row_id);
                match check_relevance(&client, &gemini_url, title, &snippet, &service_category, &config.target_city).await {
                    Ok(true) => {
                        sqlx::query("UPDATE contractors SET lifecycle_state = 1 WHERE id = $1")
                            .bind(&row_id)
                            .execute(pool)
                            .await?;
                        println!("[Sanitizer] Relevance confirmed for '{}'. Promoted clean record ID {} to state 1.", name, row_id);
                        
                        // Promoted clean record: insert its attributes into processed sets so subsequent records in the batch can be compared against it!
                        if let Some(dom) = domain_opt {
                            if !dom.is_empty() {
                                processed_domains.insert(dom);
                            }
                        }
                        if let Some(ref p) = phone {
                            let normalized = p.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
                            if !normalized.is_empty() && normalized != "unknown" {
                                processed_phones.insert(normalized);
                            }
                        }
                        let slug = name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "-");
                        let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<&str>>().join("-");
                        if !slug.is_empty() {
                            processed_slugs.insert(slug);
                        }
                    }
                    Ok(false) => {
                        sqlx::query("UPDATE contractors SET lifecycle_state = -3, outreach_status = 'discarded' WHERE id = $1")
                            .bind(&row_id)
                            .execute(pool)
                            .await?;
                        println!("DISCARDED: Non-relevant entity '{}' in the database (ID: {})", name, row_id);
                    }
                    Err(e) => {
                        eprintln!("[Sanitizer Error] Agentic relevance check failed for '{}' (ID: {}): {}. Keeping at state 0 to retry later.", name, row_id, e);
                    }
                }
            }
        }
    }

    Ok(())
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

fn is_local_phone(phone: &str, config: &crate::MarketConfig) -> bool {
    let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
    let normalized = if digits.starts_with('1') && digits.len() == 11 {
        &digits[1..]
    } else {
        &digits
    };
    if let Some(ref codes) = config.local_area_codes {
        codes.iter().any(|code| normalized.starts_with(code))
    } else {
        normalized.starts_with("360") || normalized.starts_with("564")
    }
}

async fn check_relevance(
    client: &reqwest::Client,
    gemini_url: &str,
    title: &str,
    snippet: &str,
    niche: &str,
    city: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let payload = json!({
        "contents": [{
            "parts": [{
                "text": format!(
                    "Analyze the following website information (Title and markdown snippet):\n\n\
                     Title: {}\n\n\
                     Content Snippet:\n```markdown\n{}\n```\n\n\
                     Does this content describe a physical business entity in the {} niche, located in {}? Answer only YES or NO. If the page is a ToS, Login, or Privacy Policy, answer NO.",
                    title, snippet, niche, city
                )
            }]
        }],
        "systemInstruction": {
            "parts": [{
                "text": "You are a data validation filter. Analyze the provided website content and classify if it belongs to a physical business entity matching the niche and city. Output only YES or NO. Do not add any punctuation, conversational text, or explanation."
            }]
        },
        "generationConfig": {
            "responseMimeType": "text/plain"
        }
    });

    let response = client
        .post(gemini_url)
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Gemini API returned error status: {}. Body: {}", status, body).into());
    }

    let json_resp: Value = response.json().await?;
    let text = json_resp
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|cand| cand.get("content"))
        .and_then(|cont| cont.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())
        .and_then(|part| part.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();

    Ok(text.to_uppercase().contains("YES"))
}
