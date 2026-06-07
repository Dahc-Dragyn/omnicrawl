# Omnicrawl

**Omnicrawl** is a high-performance, modular Rust appliance designed to rapidly ingest target web URLs, refine data deterministically, extract operational matrices using LLMs, and compile zero-JS, high-trust SEO pillar pages.

It transforms a raw list of competitor or local business URLs into a deployed, Lighthouse 100/100 optimized HTML directory.

---

## 🏗️ Core Architecture & Pipeline

Omnicrawl operates on a strict, state-machine-driven PostgreSQL backend (configured via `DATABASE_URL`). The pipeline is separated into six deterministic phases:

0. **Phase 0: Auto-Discovery (`discovery.rs`)**
   - Automatically queries search engines and discovery directories based on categories and target markets in `market.toml`.
   - Populates database records with discovered targets at `lifecycle_state = 0`. Legacy seed injection fallbacks have been completely purged to guarantee geographic purity.

1. **Phase 1: DB Abstraction & Initialization (`db.rs`)**
   - Establishes a connection to the high-concurrency PostgreSQL backend database using connection pooling.
   - Mock data and hardcoded seeds are completely excluded during initialization to maintain database cleanliness.

2. **Phase 2: Live Web Ingestion (`crawler.rs`)**
   - Scrapes targets in the database queue using a localized **Firecrawl** Docker cluster.
   - Extracts semantic markdown, initial names, and E.164 normalized phone numbers.

3. **Phase 3: Deterministic Refinement (`refinement.rs`)**
   - Sweeps the database for state `0` records.
   - Applies the **Jaro-Winkler string similarity algorithm** (threshold: `> 0.88`) to prevent duplicate entities (duplicates flagged as `state = -1`).
   - Runs a strict **Geo-Authority and Locality Filter (GeoScore)**: evaluates phone area codes against the market's approved prefixes (e.g. `360`/`564` yields `+100` for Vancouver, other area codes yield `-500`), checks for CCPH Clark County Public Health licensing (`+100`), and verifies physical presence via explicit mentions of the target city and state.
   - Quarantines non-local/unverified entries (`state = -2`) and promotes clean local records to `lifecycle_state = 1`.

4. **Phase 4: Token-Optimized LLM Enrichment (`enrichment.rs`)**
   - Sends the unstructured markdown payload of state `1` records to the **Google Gemini 3.1 Flash-Lite API**.
   - Extract operational schemas (materials, job size limits, licensing validation, certification badges, etc.).
   - Promotes enriched records to `lifecycle_state = 2`.

5. **Phase 5: Static Asset & SEO Compilation (`compiler.rs`)**
   - **Automated Directory Purge**: Safely deletes all stale `.html` assets and `sitemap.xml` from the market's specific directory in `dist/` at startup to prevent "ghost" listings from persisting across runs.
   - **Omni-Trap Honeypot Labyrinth**: Automatically generates 5 recursively linked HTML honeypots inside the market's `dist/{domain}/trap/` directory containing high-density keywords to bait scrapers, injects an invisible entry link in the footer of all compiled business pages, outputs a server-side IP fingerprint logger (`trap.php`), and writes a `robots.txt` configuration disallowing `/trap/` to protect compliant search crawlers.
   - **Programmatic Sentence Variation**: Generates unique `intro_text`, `meta_description` (under 160 characters), and `about_text` using deterministic length-offset modulo indexing on business names to block search engine "Scaled Content Abuse" penalties while maintaining stable crawl histories.
   - **Rich Schema Snippets**: Injects valid Schema.org `LocalBusiness` JSON-LD schemas, mapping categories to specific subtypes (`Plumber`, `Electrician`, `GeneralContractor`, `HVACBusiness`) and outputting aggregate rating and review counters.
   - **IndexNow API Push-Indexing**: Writes the IndexNow 32-character hex key file to the market's build root and issues an async HTTP POST request to IndexNow to push-index all newly generated URLs instantly.
   - Promotes published records to `lifecycle_state = 4`.

---

## ⚙️ Central Configuration (`market.toml`)

Omnicrawl relies on a central `market.toml` configuration file in the project root to drive variable insertion across both Rust and Python modules. It supports looping through multiple markets sequentially:

```toml
# central configuration for Omnicrawl Multi-Market Orchestrator

[[markets]]
target_city = "Vancouver"
target_state = "WA"
network_name = "Vancouver Professional Services"
target_niches = ["kitchen_remodel", "plumbing", "electrical", "pest_control", "septic_system", "hvac", "roofing", "painting", "landscaping", "drywall_repair"] 
network_domain = "vancouver.aiyoda.com"
indexnow_key = "6b75c13b2c1248ff8d9a4b3d168fe2d9"
local_area_codes = ["360", "564"]

[[markets]]
target_city = "Portland"
target_state = "OR"
network_name = "Portland Elite Services"
target_niches = ["kitchen_remodel", "plumbing", "electrical", "hvac", "roofing", "painting", "landscaping"]
network_domain = "portland.aiyoda.com"
indexnow_key = "7c75c13b2c1248ff8d9a4b3d168fe2d0"
local_area_codes = ["503", "971"]
```

---

## 🚀 Prerequisites

Before running Omnicrawl, ensure your environment is configured:

- **Rust / Cargo**: Latest stable toolchain.
- **PostgreSQL**: An active PostgreSQL server accessible via `DATABASE_URL`.
- **Firecrawl**: Must be running locally on `localhost:3002`.
  ```bash
  cd firecrawl
  docker compose up -d
  ```
- **Gemini API Key**: Set your Google AI Studio API key in your terminal session.
  ```powershell
  $env:GEMINI_API_KEY="YOUR_API_KEY_HERE"
  ```
- **Database URL**: Set the connection string in your terminal session.
  ```powershell
  $env:DATABASE_URL="postgres://postgres:password@localhost:5432/postgres"
  ```

---

## 📖 Standard Operating Procedure (SOP)

### 1. Verify API Connection
Before running a large ingestion crawl, verify that your Gemini API key is functional using the connectivity check tool:
```bash
cargo run --manifest-path "C:\Antigravity projects\Rust\omnicrawl\target_bravo\Cargo.toml" --bin connectivity_check
```

### 2. Execute the Ingestion/Compilation Pipeline
Run the engine from the workspace target directory. The pipeline automatically reads your configuration and processes discovered entries:
```bash
cargo run --manifest-path "C:\Antigravity projects\Rust\omnicrawl\target_bravo\Cargo.toml"
```

### 3. Database Queue Reset (Resumption)
To re-run the refinement and enrichment phases on existing records without re-crawling target sites, reset the lifecycle state of processed contractors back to state 0:
```sql
UPDATE contractors SET lifecycle_state = 0 WHERE lifecycle_state > -2;
```

### 4. Deploy
Deploy compiled files inside `dist/{domain}/` directly to your web servers, CDN, or Cloudflare storage bucket for each domain.

---

## 🤖 Lead Dispatcher, outreach & Monetization

Omnicrawl implements a full monetization stack to capture and process commercial leads:

### 1. FastMCP Server (`server.py`)
Exposes the database to AI agents via stdio to support category queries and capability checks (e.g., financing, estimates, emergency service badges) over the active PostgreSQL data layer.

### 2. Conversational Dispatcher API (`api.py` / `dispatcher.py`)
An asynchronous FastAPI server that maps incoming customer queries to matched local contractors. If no contractors match, it pivots gracefully to asking logistics questions and returns DIY mitigation tips. Promoted leads are logged to `leads_generated.log`.

### 3. Automated Cold-Outreach Engine (`outreach_engine.py`)
A standalone Python script that automates B2B cold outreach.
* Queries the PostgreSQL database for unclaimed contractors containing valid, non-empty email addresses in their enriched payloads.
* Generates personalized B2B email proposals matching target profile names, categories, cities, and live compiled profile URLs.
* Writes drafted outputs (To, Subject, and Body) to `outreach_dry_run.txt`.

To run outreach:
```powershell
$env:DATABASE_URL="postgres://postgres:password@localhost:5432/postgres"
python "C:\Antigravity projects\Rust\omnicrawl\target_bravo\outreach_engine.py"
```

---

## 📂 Directory Structure

* `/src`: Core discovery, refinement, and enrichment modules.
* `/target_bravo`: Active target directory.
* `/dist`: Output directory containing subdirectories named after configured `network_domain` values (e.g. `dist/vancouver.aiyoda.com/`).
* `/templates`: Askama templates (`business_page.html`, `index.html`, `sitemap.xml`).
* `outreach_engine.py`: Cold-outreach script.
* `outreach_dry_run.txt`: Drafted email outreach output.
* `/mcp_server`: Contains FastMCP, FastAPI dispatcher, Dockerfile, and lead tracking.
* `market.toml`: Central directory configuration.
