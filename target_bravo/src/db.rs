use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use std::str::FromStr;
use sqlx::Row;
use std::collections::HashSet;

pub async fn init_db(db_url: &str) -> Result<PgPool, sqlx::Error> {
    let options = PgConnectOptions::from_str(db_url)?;

    // Configure connection pool with min and max connections for concurrency support
    let pool = PgPoolOptions::new()
        .min_connections(5)
        .max_connections(50)
        .connect_with(options)
        .await?;
    
    // Create unified tables
    create_tables(&pool).await?;

    Ok(pool)
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

async fn create_tables(pool: &PgPool) -> Result<(), sqlx::Error> {
    // 1. contractors table using JSONB for metadata structures
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS contractors (
            id TEXT PRIMARY KEY,
            contractor_license_number TEXT,
            legal_entity_name TEXT NOT NULL,
            zip_code TEXT,
            primary_vertical TEXT,
            phone_number TEXT,
            website_url TEXT,
            lifecycle_state INTEGER DEFAULT 0,
            enriched_data JSONB,
            service_category TEXT NOT NULL DEFAULT 'kitchen_remodel',
            target_city TEXT NOT NULL DEFAULT 'Vancouver',
            operational_matrix JSONB DEFAULT '{}'::jsonb,
            outreach_status TEXT DEFAULT 'uncontacted',
            last_outreached_at TIMESTAMP,
            daily_lead_cap INTEGER DEFAULT 5,
            last_lead_sent_at TIMESTAMP,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );"
    )
    .execute(pool)
    .await?;

    // 2. license_data table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS license_data (
            contractor_id TEXT PRIMARY KEY,
            license_status TEXT,
            expiration_date TEXT,
            liability_insurance_carrier TEXT,
            surety_bond_firm TEXT,
            bond_amount REAL,
            FOREIGN KEY(contractor_id) REFERENCES contractors(id) ON DELETE CASCADE
        );"
    )
    .execute(pool)
    .await?;

    // 3. seo_metrics table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS seo_metrics (
            contractor_id TEXT PRIMARY KEY,
            mobile_load_speed_ms INTEGER,
            has_local_business_schema BOOLEAN,
            fragility_score REAL,
            FOREIGN KEY(contractor_id) REFERENCES contractors(id) ON DELETE CASCADE
        );"
    )
    .execute(pool)
    .await?;

    // 4. leads table
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS leads (
            id UUID PRIMARY KEY,
            contractor_id TEXT REFERENCES contractors(id) ON DELETE SET NULL,
            lead_payload JSONB,
            lead_score REAL,
            status TEXT DEFAULT 'assigned',
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );"
    )
    .execute(pool)
    .await?;

    // Programmatic Purge of generic junk / failed scrapes to clean up the database
    let purge_res = sqlx::query(
        "DELETE FROM contractors 
         WHERE legal_entity_name LIKE '%Unknown Contractor%' 
            OR legal_entity_name LIKE '%Error%'
            OR legal_entity_name = 'Unknown'
            OR legal_entity_name = 'error'
            OR legal_entity_name = '';"
    )
    .execute(pool)
    .await?;
    
    if purge_res.rows_affected() > 0 {
        println!("[Database] Purged {} generic/failed stale directory listings from database.", purge_res.rows_affected());
    }

    // Programmatic Reset: Shift published (state 4) listings to state 1 to trigger LLM contact re-enrichment
    let reset_res = sqlx::query(
        "UPDATE contractors 
         SET lifecycle_state = 1 
         WHERE lifecycle_state = 4;"
    )
    .execute(pool)
    .await?;

    if reset_res.rows_affected() > 0 {
        println!("[Database] Reset {} published listings back to state 1 for dynamic brand-name re-enrichment.", reset_res.rows_affected());
    }

    // Programmatic Deduplication: Purge duplicate domain, phone, and name slug records from the existing database
    let all_contractors = sqlx::query("SELECT id, legal_entity_name, website_url, phone_number, lifecycle_state FROM contractors;")
        .fetch_all(pool)
        .await?;
    
    // Sort descending by state so we keep the most fully processed, published, or enriched record (state 4 / 2) over raw state 0
    let mut contractors_list: Vec<(String, String, Option<String>, Option<String>, i32)> = all_contractors.into_iter().map(|row| {
        let id: String = row.get("id");
        let name: String = row.get("legal_entity_name");
        let website: Option<String> = row.get("website_url");
        let phone: Option<String> = row.get("phone_number");
        let state: i32 = row.get("lifecycle_state");
        (id, name, website, phone, state)
    }).collect();
    
    contractors_list.sort_by(|a, b| b.4.cmp(&a.4));

    let mut seen_domains = HashSet::new();
    let mut seen_phones = HashSet::new();
    let mut seen_slugs = HashSet::new();
    let mut duplicate_ids = Vec::new();

    for (id, name, website, phone, _state) in contractors_list {
        let mut is_dup = false;

        // 1. Deduplicate by root domain
        if let Some(ref w) = website {
            if let Some(domain) = extract_domain(w) {
                if !domain.is_empty() {
                    if seen_domains.contains(&domain) {
                        is_dup = true;
                    } else {
                        seen_domains.insert(domain);
                    }
                }
            }
        }

        // 2. Deduplicate by normalized phone number
        if !is_dup {
            if let Some(ref p) = phone {
                let normalized = p.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
                if !normalized.is_empty() && normalized != "unknown" {
                    if seen_phones.contains(&normalized) {
                        is_dup = true;
                    } else {
                        seen_phones.insert(normalized);
                    }
                }
            }
        }

        // 3. Deduplicate by clean name slug
        if !is_dup {
            let slug = name.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "-");
            let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<&str>>().join("-");
            if !slug.is_empty() {
                if seen_slugs.contains(&slug) {
                    is_dup = true;
                } else {
                    seen_slugs.insert(slug);
                }
            }
        }

        if is_dup {
            duplicate_ids.push(id);
        }
    }

    if !duplicate_ids.is_empty() {
        println!("[Database] Found {} existing duplicate domain/phone/slug records. Purging...", duplicate_ids.len());
        for id in duplicate_ids {
            sqlx::query("DELETE FROM contractors WHERE id = $1;")
                .bind(&id)
                .execute(pool)
                .await?;
        }
        println!("[Database] Duplicate purge completed cleanly.");
    }

    // Safe dynamic migration: Check if service_category exists in existing databases using standard PG catalog
    let column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'service_category';"
    )
    .fetch_all(pool)
    .await?;

    if column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN service_category TEXT NOT NULL DEFAULT 'kitchen_remodel';")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added service_category column to contractors table.");
    }

    // Safe dynamic migration: Check if target_city exists in existing databases
    let city_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'target_city';"
    )
    .fetch_all(pool)
    .await?;

    if city_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN target_city TEXT NOT NULL DEFAULT 'Vancouver';")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added target_city column to contractors table.");
    }

    // Safe dynamic migration: Check if operational_matrix exists in existing databases
    let matrix_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'operational_matrix';"
    )
    .fetch_all(pool)
    .await?;

    if matrix_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN operational_matrix JSONB DEFAULT '{}'::jsonb;")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added operational_matrix column to contractors table.");
    }

    // Safe dynamic migration: Check if outreach_status exists in existing databases
    let status_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'outreach_status';"
    )
    .fetch_all(pool)
    .await?;

    if status_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN outreach_status TEXT DEFAULT 'uncontacted';")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added outreach_status column to contractors table.");
    }

    // Safe dynamic migration: Check if last_outreached_at exists in existing databases
    let last_out_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'last_outreached_at';"
    )
    .fetch_all(pool)
    .await?;

    if last_out_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN last_outreached_at TIMESTAMP;")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added last_outreached_at column to contractors table.");
    }

    // Safe dynamic migration: Check if daily_lead_cap exists in existing databases
    let cap_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'daily_lead_cap';"
    )
    .fetch_all(pool)
    .await?;

    if cap_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN daily_lead_cap INTEGER DEFAULT 5;")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added daily_lead_cap column to contractors table.");
    }

    // Safe dynamic migration: Check if last_lead_sent_at exists in existing databases
    let last_lead_column_check = sqlx::query(
        "SELECT 1 FROM information_schema.columns 
         WHERE table_name = 'contractors' AND column_name = 'last_lead_sent_at';"
    )
    .fetch_all(pool)
    .await?;

    if last_lead_column_check.is_empty() {
        sqlx::query("ALTER TABLE contractors ADD COLUMN last_lead_sent_at TIMESTAMP;")
            .execute(pool)
            .await?;
        println!("[Database] Migration succeeded: Added last_lead_sent_at column to contractors table.");
    }

    // Programmatic Demotion: If a contractor record exists at state 1 or 2 but its raw_markdown field is empty/null, demote to 0
    let demote_res = sqlx::query(
        "UPDATE contractors 
         SET lifecycle_state = 0 
         WHERE lifecycle_state IN (1, 2) 
           AND (
               enriched_data->>'raw_markdown' IS NULL 
               OR enriched_data->>'raw_markdown' = ''
           );"
    )
    .execute(pool)
    .await?;

    if demote_res.rows_affected() > 0 {
        println!("[Database] Demoted {} records with empty raw_markdown back to lifecycle_state = 0 for re-crawling.", demote_res.rows_affected());
    }

    // Add high-performance query indexes
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_contractors_lifecycle ON contractors(lifecycle_state);").execute(pool).await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_contractors_license ON contractors(contractor_license_number);").execute(pool).await?;

    Ok(())
}
