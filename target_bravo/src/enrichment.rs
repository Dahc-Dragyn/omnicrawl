use sqlx::{PgPool, Row};
use sqlx::types::Json;
use serde::{Serialize, Deserialize};
use serde_json::{json, Value};
use std::env;

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction")]
    system_instruction: GeminiSystemInstruction,
    #[serde(rename = "generationConfig")]
    generation_config: GeminiConfig,
}

#[derive(Serialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GeminiSystemInstruction {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiConfig {
    #[serde(rename = "responseMimeType")]
    response_mime_type: String,
    #[serde(rename = "responseSchema")]
    response_schema: Value,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: Option<GeminiResponseContent>,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    parts: Option<Vec<GeminiResponsePart>>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: Option<String>,
}

pub async fn enrich_contractors(pool: &PgPool, config: &crate::MarketConfig) -> Result<(), Box<dyn std::error::Error>> {
    println!("[Enrichment] Initiating Phase 4 (LLM Enrichment) for market {}...", config.target_city);

    // Query the database for records sitting at lifecycle_state = 1 for this market
    let rows = sqlx::query(
        "SELECT id, website_url, legal_entity_name, phone_number, enriched_data FROM contractors WHERE lifecycle_state = 1 AND target_city = $1"
    )
    .bind(&config.target_city)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        println!("[Enrichment] No records sitting at lifecycle_state = 1 found for market {}. Enrichment complete.", config.target_city);
        return Ok(());
    }

    println!("[Enrichment] Found {} contractors to enrich.", rows.len());

    // Load Gemini API Key
    let api_key = env::var("GEMINI_API_KEY")
        .or_else(|_| env::var("GOOGLE_API_KEY"))
        .map_err(|_| "GEMINI_API_KEY or GOOGLE_API_KEY environment variable must be set for enrichment phase")?;

    let client = reqwest::Client::new();
    
    // Model Alignment: Configure target for gemini-3.1-flash-lite
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent?key={}",
        api_key
    );

    // Define strict JSON Schema for Boolean Matrix output
    let response_schema = json!({
        "type": "object",
        "properties": {
            "core_features": {
                "type": "object",
                "properties": {
                    "has_24_7_emergency": {"type": "boolean"},
                    "offers_financing": {"type": "boolean"},
                    "offers_free_estimates": {"type": "boolean"},
                    "is_family_owned": {"type": "boolean"},
                    "eco_friendly_practices": {"type": "boolean"}
                },
                "required": ["has_24_7_emergency", "offers_financing", "offers_free_estimates", "is_family_owned", "eco_friendly_practices"]
            },
            "niche_features": {
                "type": "object",
                "properties": {
                    "handles_septic": {"type": "boolean"},
                    "heat_pump_specialist": {"type": "boolean"},
                    "cabinet_refacing": {"type": "boolean"}
                },
                "required": ["handles_septic", "heat_pump_specialist", "cabinet_refacing"]
            },
            "commercial_focus": {
                "type": "object",
                "properties": {
                    "serves_residential": {"type": "boolean"},
                    "serves_commercial": {"type": "boolean"}
                },
                "required": ["serves_residential", "serves_commercial"]
            },
            "trust_signals": {
                "type": "object",
                "properties": {
                    "explicit_warranties_mentioned": {"type": "boolean"},
                    "years_in_business": {"type": "integer", "nullable": true}
                },
                "required": ["explicit_warranties_mentioned", "years_in_business"]
            }
        },
        "required": ["core_features", "niche_features", "commercial_focus", "trust_signals"]
    });

    for row in rows {
        // Map fields dynamically from row
        let row_id: String = row.get("id");
        let legal_entity_name: String = row.get("legal_entity_name");
        let db_enriched_json = row.get::<Option<Json<Value>>, _>("enriched_data");

        println!(
            "[Enrichment] Requesting extraction for contractor: '{}' (ID: {})...",
            legal_entity_name, row_id
        );

        // Parse existing enriched_data JSON to retrieve the raw markdown
        let db_enriched: Value = db_enriched_json.map(|j| j.0).unwrap_or_else(|| json!({}));

        let raw_markdown = db_enriched
            .get("raw_markdown")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if raw_markdown.is_empty() {
            eprintln!("[Enrichment Warning] raw_markdown is empty for contractor ID {}. Demoting to lifecycle_state = 0 for re-crawling...", row_id);
            sqlx::query("UPDATE contractors SET lifecycle_state = 0 WHERE id = $1")
                .bind(&row_id)
                .execute(pool)
                .await?;
            continue;
        }

        // Construct structured payload enforcing strict zero-inference rules
        let prompt_text = format!(
            "Analyze the following contractor website markdown content and extract the operational matrix structure.\n\n\
            Zero-Inference Rule: If the text does not explicitly state or strongly imply a feature, default to false. Do not guess.\n\n\
            Website Content:\n\n```markdown\n{}\n```\n",
            raw_markdown
        );

        let system_instruction_text = "You are a professional local directory data extraction agent. \
            Analyze the provided contractor website text and extract their operational matrix in strict compliance with the response schema. \
            Follow the 'Zero-Inference Rule' strictly: If the text does not explicitly state or strongly imply a feature, default to false. Do not guess or infer. \
            No conversational hedging, no markdown code block formatting in the output, just return the raw JSON matching the schema precisely.";

        let payload = GeminiRequest {
            contents: vec![GeminiContent {
                parts: vec![GeminiPart { text: prompt_text }],
            }],
            system_instruction: GeminiSystemInstruction {
                parts: vec![GeminiPart { text: system_instruction_text.to_string() }],
            },
            generation_config: GeminiConfig {
                response_mime_type: "application/json".to_string(),
                response_schema: response_schema.clone(),
            },
        };

        let response = client
            .post(&url)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let err_body = response.text().await.unwrap_or_default();
            eprintln!(
                "[Enrichment Error] Gemini API returned error status: {} for contractor ID {}. Body: {}",
                status,
                row_id,
                err_body
            );
            continue;
        }

        let gemini_res: GeminiResponse = response.json().await?;
        
        let response_text = gemini_res
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|cand| cand.content)
            .and_then(|cont| cont.parts)
            .and_then(|parts| parts.into_iter().next())
            .and_then(|part| part.text)
            .unwrap_or_default();

        if response_text.is_empty() {
            eprintln!("[Enrichment Error] Empty candidate response from Gemini API for ID {}", row_id);
            continue;
        }

        // Deserialize Gemini structured output safely
        let operational_matrix: Value = match serde_json::from_str(&response_text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "[Enrichment Error] Failed to deserialize Gemini JSON output: {}. Raw response: {}",
                    e, response_text
                );
                // Default/empty JSON structure for malformed/parsing failures
                json!({"error": "parsing_failed"})
            }
        };

        println!(
            "[Enrichment] Successfully extracted operational matrix for {}: {}",
            legal_entity_name, operational_matrix
        );

        let operational_matrix_str = serde_json::to_string(&operational_matrix)?;
        println!("[DB] Saving operational matrix for {} to Postgres...", legal_entity_name);

        // Update database: inject structured JSON to operational_matrix, and advance state to 2 (Enriched)
        match sqlx::query(
            "UPDATE contractors 
             SET operational_matrix = $1::jsonb, lifecycle_state = 2 
             WHERE id = $2"
        )
        .bind(&operational_matrix_str)
        .bind(&row_id)
        .execute(pool)
        .await
        {
            Ok(_) => {
                println!("[DB] Save successful");
                println!("[Enrichment] Committed updated record (lifecycle_state = 2) for ID {}!", row_id);
            }
            Err(e) => {
                println!("[DB] Save failed: {}", e);
                eprintln!("[Enrichment Error] Failed to update database for contractor ID {}: {}", row_id, e);
            }
        }
    }

    Ok(())
}
