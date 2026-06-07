use std::env;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[Diagnostic] Starting Gemini API Connectivity Check...");

    // Check GEMINI_API_KEY environment variable, fallback to GOOGLE_API_KEY
    let api_key = env::var("GEMINI_API_KEY")
        .or_else(|_| env::var("GOOGLE_API_KEY"))
        .map_err(|_| "GEMINI_API_KEY or GOOGLE_API_KEY environment variable is not set")?;

    println!("[Diagnostic] Using API Key (length: {})...", api_key.len());

    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.1-flash-lite:generateContent?key={}",
        api_key
    );

    let payload = json!({
        "contents": [{
            "parts": [{ "text": "Hello" }]
        }]
    });

    println!("[Diagnostic] Dispatching probe request to Gemini API...");
    match client.post(&url).json(&payload).send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                println!("[Diagnostic] SUCCESS: Gemini API connection is functional.");
            } else {
                println!("[Diagnostic] FAILURE: Gemini API returned status code: {}", status);
                println!("[Diagnostic] Response body: {}", body);
            }
        }
        Err(e) => {
            println!("[Diagnostic] ERROR: Failed to connect to Gemini API: {}", e);
        }
    }

    Ok(())
}
