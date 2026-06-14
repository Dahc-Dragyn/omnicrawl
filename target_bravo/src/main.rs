mod db;
mod crawler;
mod enrichment;
mod refinement;
mod compiler;
mod discovery;

// Global atomic counters for Gemini API token and cost tracking
pub static INPUT_TOKENS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static OUTPUT_TOKENS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static API_CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn print_run_receipt() {
    let input = INPUT_TOKENS.load(std::sync::atomic::Ordering::SeqCst);
    let output = OUTPUT_TOKENS.load(std::sync::atomic::Ordering::SeqCst);
    let calls = API_CALLS.load(std::sync::atomic::Ordering::SeqCst);

    let input_cost = (input as f64 / 1_000_000.0) * 0.075;
    let output_cost = (output as f64 / 1_000_000.0) * 0.30;
    let total_cost = input_cost + output_cost;

    println!("=========================================");
    println!("        OMNICRAWL RUN RECEIPT          ");
    println!("=========================================");
    println!(" API Calls Made: {}", calls);
    println!(" Input Tokens:   {}", input);
    println!(" Output Tokens:  {}", output);
    println!("-----------------------------------------");
    println!(" Estimated Cost: ${:.6}", total_cost);
    println!("=========================================");
}

use std::sync::Arc;
use tokio::sync::Semaphore;
use sqlx::Row;

#[derive(serde::Deserialize, Clone, Debug)]
pub struct MarketConfig {
    pub target_city: String,
    pub target_state: String,
    pub network_name: String,
    pub target_niches: Vec<String>,
    pub network_domain: Option<String>,
    pub indexnow_key: Option<String>,
    pub local_area_codes: Option<Vec<String>>,
}

#[derive(serde::Deserialize, Debug)]
struct MultiMarketConfig {
    markets: Vec<MarketConfig>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[Target Bravo] Initializing PostgreSQL database pipeline...");

    // Connection string for PostgreSQL, driven by env with a local fallback
    let db_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@localhost:5432/postgres".to_string());

    println!("[Engine] Connecting to PostgreSQL at: {}", db_url);

    // Initialize database and connection pool once for all markets
    let pool = db::init_db(&db_url).await?;

    // Load multi-market configuration
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let config_path = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap_or(std::path::Path::new(manifest_dir))
        .join("market.toml");

    if !config_path.exists() {
        return Err(format!("market.toml not found at project root: {}", config_path.display()).into());
    }

    let config_content = std::fs::read_to_string(&config_path)?;
    let multi_config: MultiMarketConfig = toml::from_str(&config_content)?;

    println!("[Engine] Loaded {} markets from configuration.", multi_config.markets.len());

    for market in multi_config.markets {
        println!("\n========================================================");
        println!("[Engine] Starting Pipeline for Market: {}, {}", market.target_city, market.target_state);
        println!("========================================================\n");

        // Run Auto-Discovery Module
        if let Err(e) = discovery::run_discovery(&pool, &market).await {
            eprintln!("[Discovery Error] Phase 0 Auto-Discovery failed for {}: {}", market.target_city, e);
        } else {
            println!("[Target Bravo] Phase 0: Auto-Discovery Ingestion Completed Cleanly for {}!", market.target_city);
        }

        // Run a diagnostic count of existing database records for this market and print them
        let diag_rows = sqlx::query("SELECT legal_entity_name, service_category, lifecycle_state FROM contractors WHERE target_city = $1")
            .bind(&market.target_city)
            .fetch_all(&pool)
            .await?;
        println!("[DIAGNOSTIC] Current Database Inventory for {} ({} records total):", market.target_city, diag_rows.len());
        for row in &diag_rows {
            let name: String = row.get("legal_entity_name");
            let cat: String = row.get("service_category");
            let state: i32 = row.get("lifecycle_state");
            println!("  -> Contractor: '{}' | Category: '{}' | State: {}", name, cat, state);
        }

        // Query database for any contractors in lifecycle_state = 0 belonging to this market
        let pending_db_rows = sqlx::query("SELECT website_url, service_category FROM contractors WHERE lifecycle_state = 0 AND target_city = $1")
            .bind(&market.target_city)
            .fetch_all(&pool)
            .await?;

        let mut targets = Vec::new();
        for row in pending_db_rows {
            let url_opt: Option<String> = row.get("website_url");
            let category: String = row.get("service_category");
            if let Some(url) = url_opt {
                let trimmed = url.trim();
                if !trimmed.is_empty() && trimmed != "#" {
                    println!("[Database Queue] Found pending/discovered target to crawl: {}", trimmed);
                    targets.push((trimmed.to_string(), category));
                }
            }
        }

        println!("[Target Bravo] Preparing to ingest {} live targets under rate-limited safeguards for {}...", targets.len(), market.target_city);

        // Enforce high-concurrency protection using a Tokio Semaphore (limit = 2 concurrent requests)
        let semaphore = Arc::new(Semaphore::new(2));
        let mut handles = vec![];

        for (target_url, category) in targets {
            let pool_clone = pool.clone();
            let sem_clone = semaphore.clone();
            let market_clone = market.clone();
            
            let handle = tokio::spawn(async move {
                // Acquire permit from the semaphore
                let _permit = sem_clone.acquire().await.expect("Failed to acquire semaphore permit");
                
                // Execute crawler scraping layer with assigned category and current market
                if let Err(e) = crawler::scrape_target_url(&pool_clone, &target_url, &category, &market_clone).await {
                    eprintln!("[Crawler Error] Failed to scrape target '{}' (category: {}): {}", target_url, category, e);
                }
            });
            
            handles.push(handle);
        }

        // Await all background crawler workers
        for handle in handles {
            let _ = handle.await;
        }

        println!("[Target Bravo] Phase 2: Live Ingestion Cycle Completed Cleanly for {}!", market.target_city);

        // Execute Phase 3: Deterministic Refinement Loop
        if let Err(e) = refinement::refine_contractors(&pool, &market).await {
            eprintln!("[Refinement Error] Phase 3 refinement failed for {}: {}", market.target_city, e);
        } else {
            println!("[Target Bravo] Phase 3: Refinement Loop Completed Cleanly for {}!", market.target_city);
        }

        // Execute Phase 4: Token-Optimized LLM Enrichment
        if let Err(e) = enrichment::enrich_contractors(&pool, &market).await {
            eprintln!("[Enrichment Error] Phase 4 LLM enrichment failed for {}: {}", market.target_city, e);
        } else {
            println!("[Target Bravo] Phase 4: Live LLM Enrichment Completed Cleanly for {}!", market.target_city);
        }

        // Execute Phase 5: Static Asset & SEO Compilation
        if let Err(e) = compiler::compile_static_assets(&pool, &market).await {
            eprintln!("[Compiler Error] Phase 5 Compilation failed for {}: {}", market.target_city, e);
        } else {
            println!("[Target Bravo] Phase 5: Compilation Completed Cleanly for {}!", market.target_city);
        }
    }

    print_run_receipt();

    Ok(())
}
