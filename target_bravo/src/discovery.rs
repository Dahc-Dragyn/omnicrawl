use sqlx::{PgPool, Row};
use uuid::Uuid;
use regex::Regex;
use serde_json::json;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

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
}

#[derive(serde::Serialize)]
struct SearchRequest<'a> {
    query: &'a str,
    limit: Option<usize>,
}

#[derive(serde::Deserialize)]
struct SearchResponse {
    success: Option<bool>,
    data: Option<Vec<SearchItem>>,
}

#[derive(serde::Deserialize)]
struct SearchItem {
    url: Option<String>,
}

use crate::MarketConfig;

pub async fn run_discovery(pool: &PgPool, config: &MarketConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("[Discovery] Initiating Phase 0 (Auto-Discovery Ingestion) for city: {}...", config.target_city);

    if config.target_niches.is_empty() {
        println!("[Discovery] No niches listed for market. Skipping auto-discovery.");
        return Ok(());
    }

    println!(
        "[Discovery] Loaded central configuration for City: {}, State: {}, Niches: {:?}", 
        config.target_city, config.target_state, config.target_niches
    );

    println!("[Discovery] Starting discovery for city: {}", config.target_city);

    // Fetch existing domains from the database to prevent duplicate ingestion for this market
    let existing_rows = sqlx::query("SELECT website_url FROM contractors WHERE target_city = $1")
        .bind(&config.target_city)
        .fetch_all(pool)
        .await?;
    
    let mut existing_domains = HashSet::new();
    for row in existing_rows {
        if let Some(url) = row.get::<Option<String>, _>("website_url") {
            if let Some(dom) = extract_domain(&url) {
                existing_domains.insert(dom);
            }
        }
    }

    let client = reqwest::Client::new();
    let url_regex = Regex::new(r#"https?://[a-zA-Z0-9.-]+\.[a-zA-Z]{2,6}(?:/[^\s()<>\[\]"']*)?"#)?;

    for niche in &config.target_niches {
        let clean_seed = format!("{} contractors {} {}", niche, config.target_city, config.target_state);
        let category = niche.clone();

        if clean_seed.is_empty() {
            continue;
        }

        println!("[Discovery] Processing seed: '{}' (Category: {})", clean_seed, category);

        let mut discovered_urls = Vec::new();

        if clean_seed.starts_with("http://") || clean_seed.starts_with("https://") {
            // Treat as root aggregator URL -> Scrape
            println!("[Discovery] Scraping root seed URL via Firecrawl...");
            let payload = ScrapeRequest {
                url: &clean_seed,
                formats: vec!["markdown"],
            };

            match client
                .post("http://localhost:3002/v1/scrape")
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_success() {
                        if let Ok(result) = resp.json::<ScrapeResponse>().await {
                            if result.success == Some(true) {
                                if let Some(data) = result.data {
                                    let markdown = data.markdown.unwrap_or_default();
                                    // Extract all URLs via Regex
                                    for mat in url_regex.find_iter(&markdown) {
                                        discovered_urls.push(mat.as_str().to_string());
                                    }
                                }
                            } else {
                                eprintln!("[Discovery Error] Firecrawl scrape was not successful for {}", clean_seed);
                            }
                        }
                    } else {
                        eprintln!("[Discovery Error] Firecrawl returned status {} for {}", resp.status(), clean_seed);
                    }
                }
                Err(e) => {
                    eprintln!("[Discovery Error] Failed to send request to Firecrawl: {}", e);
                }
            }
        } else {
            // Treat as Search Query
            println!("[Discovery] Executing deep-search query via Firecrawl...");
            let payload = SearchRequest {
                query: &clean_seed,
                limit: Some(15),
            };

            match client
                .post("http://localhost:3002/v1/search")
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_success() {
                        if let Ok(result) = resp.json::<SearchResponse>().await {
                            if result.success == Some(true) {
                                if let Some(data) = result.data {
                                    for item in data {
                                        if let Some(url) = item.url {
                                            discovered_urls.push(url);
                                        }
                                    }
                                }
                            } else {
                                eprintln!("[Discovery Error] Firecrawl search was not successful for query '{}'", clean_seed);
                            }
                        }
                    } else {
                        eprintln!("[Discovery Error] Firecrawl returned status {} for search query '{}'", resp.status(), clean_seed);
                    }
                }
                Err(e) => {
                    eprintln!("[Discovery Error] Failed to send search request to Firecrawl: {}", e);
                }
            }
        }

        // Clean, filter, and insert discovered contractor domains
        let seed_domain = extract_domain(&clean_seed).unwrap_or_default();
        let mut newly_discovered = HashSet::new();

        for url in discovered_urls {
            if let Some(domain) = extract_domain(&url) {
                if domain.is_empty() {
                    continue;
                }
                // Exclude the seed domain itself
                if !seed_domain.is_empty() && domain == seed_domain {
                    continue;
                }
                // Exclude common directories / social media
                if is_directory_or_social_media(&domain) {
                    continue;
                }
                // Exclude already existing domains in database
                if existing_domains.contains(&domain) {
                    continue;
                }
                newly_discovered.insert(domain);
            }
        }

        println!("[Discovery] Found {} new unique contractor domains from seed '{}'", newly_discovered.len(), clean_seed);

        for domain in newly_discovered {
            let contractor_id = Uuid::new_v4().to_string();
            let target_url = format!("https://{}", domain);
            let display_name = domain_to_name(&domain);

            println!("[Discovery] Found candidate: {} | {}", display_name, target_url);

            let primary_vertical = match category.as_str() {
                "plumbing" => "Plumbing Specialists".to_string(),
                "electrical" => "Electrical Services".to_string(),
                "pest_control" => "Pest Control & Extermination".to_string(),
                "septic_system" => "On-Site Septic System Services".to_string(),
                "kitchen_remodel" => "Kitchen Remodeling".to_string(),
                _ => {
                    let mut words = category.replace('_', " ");
                    if let Some(first_char) = words.chars().next() {
                        let capitalized = first_char.to_uppercase().to_string();
                        words = capitalized + &words[first_char.len_utf8()..];
                    }
                    format!("{} Services", words)
                }
            };

            let enriched_payload = json!({
                "verification_source": "auto_discovery"
            });
            let enriched_str = serde_json::to_string(&enriched_payload)?;

            println!("[DB] Saving {} to Postgres...", display_name);

            match sqlx::query(
                "INSERT INTO contractors (
                    id, legal_entity_name, website_url, primary_vertical, lifecycle_state, enriched_data, service_category, target_city
                ) VALUES ($1, $2, $3, $4, 0, $5::jsonb, $6, $7);"
            )
            .bind(&contractor_id)
            .bind(&display_name)
            .bind(&target_url)
            .bind(primary_vertical)
            .bind(&enriched_str)
            .bind(&category)
            .bind(&config.target_city)
            .execute(pool)
            .await {
                Ok(_) => {
                    println!("[DB] Save successful");
                    println!("[Discovery] Ingested new target: '{}' -> {}", display_name, target_url);
                    // Add to the local check set to avoid inserting duplicates within the same run
                    existing_domains.insert(domain);
                }
                Err(e) => {
                    println!("[DB] Save failed: {}", e);
                    eprintln!("[Discovery Error] Failed to insert contractor for domain {}: {}", domain, e);
                }
            }
        }
    }

    println!("[Discovery] Phase 0 completed successfully!");
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

fn is_directory_or_social_media(domain: &str) -> bool {
    let domain = domain.to_lowercase();
    let exclusions = [
        "yelp.com", "yellowpages.com", "yp.com", "superpages.com", "angi.com", "homeadvisor.com",
        "bbb.org", "houzz.com", "local.yahoo.com", "mapquest.com", "nextdoor.com",
        "chamberofcommerce.com", "google.com", "google.ca", "googlesyndication.com",
        "googleapis.com", "gstatic.com", "apple.com", "facebook.com", "fb.com", "instagram.com",
        "twitter.com", "t.co", "x.com", "linkedin.com", "youtube.com", "pinterest.com",
        "vimeo.com", "tiktok.com", "reddit.com", "tumblr.com", "wordpress.org", "wordpress.com",
        "wix.com", "squarespace.com", "weebly.com", "godaddy.com", "bluehost.com", "wikipedia.org",
        "schema.org", "w3.org", "github.com", "firecrawl.dev", "mendable.ai", "vancouverusa.com",
        "cityofvancouver.us", "vancouverwa.us", "wa.gov", "e-arc.com", "co.clark.wa.us",
        "forbes.com", "thumbtack.com", "expertise.com", "consumeraffairs.com", "porch.com"
    ];
    exclusions.iter().any(|&ex| domain.contains(ex))
}

fn domain_to_name(domain: &str) -> String {
    let prefix = domain.split('.').next().unwrap_or(domain);
    let mut chars = prefix.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
