#!/usr/bin/env python3
"""
Omnicrawl Outreach Engine
Queries the target_bravo PostgreSQL database for contractors with valid, unclaimed emails
and generates personalized, localized directory outreach emails.
"""

import os
import re
import json
import psycopg2

# Define directories relative to this script
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))

def load_markets_config():
    """
    Attempts to read market.toml from various directories.
    Supports tomllib (Python 3.11+), third-party toml, or falls back to a simple regex parser.
    """
    config_paths = [
        os.path.join(SCRIPT_DIR, "market.toml"),
        os.path.join(os.path.dirname(SCRIPT_DIR), "market.toml"),
        os.path.join(SCRIPT_DIR, "target_bravo", "market.toml"),
    ]
    
    markets = []
    
    for path in config_paths:
        if os.path.exists(path):
            try:
                # 1. Try python 3.11+ tomllib
                try:
                    import tomllib
                    with open(path, "rb") as f:
                        toml_data = tomllib.load(f)
                        if "markets" in toml_data:
                            print(f"[Outreach Engine] Loaded markets from {path} via tomllib")
                            return toml_data["markets"]
                except ImportError:
                    pass
                
                # 2. Try third-party toml package
                try:
                    import toml
                    with open(path, "r", encoding="utf-8") as f:
                        toml_data = toml.load(f)
                        if "markets" in toml_data:
                            print(f"[Outreach Engine] Loaded markets from {path} via toml package")
                            return toml_data["markets"]
                except ImportError:
                    pass
                
                # 3. Simple fallback parser for array of tables [[markets]]
                with open(path, "r", encoding="utf-8") as f:
                    current_market = None
                    for line in f:
                        line = line.strip()
                        if not line or line.startswith("#"):
                            continue
                        if line == "[[markets]]":
                            if current_market:
                                markets.append(current_market)
                            current_market = {}
                            continue
                        if "=" in line and current_market is not None:
                            k, v = line.split("=", 1)
                            k = k.strip()
                            v = v.strip().strip('"').strip("'")
                            if v.startswith("[") and v.endswith("]"):
                                v = [x.strip().strip('"').strip("'") for x in v[1:-1].split(",") if x.strip()]
                            current_market[k] = v
                    if current_market:
                        markets.append(current_market)
                if markets:
                    print(f"[Outreach Engine] Loaded {len(markets)} markets from {path} via fallback parser")
                    return markets
            except Exception as e:
                print(f"[Outreach Engine Warning] Error parsing {path}: {e}")
                
    print("[Outreach Engine] Using default configuration values.")
    return [{
        "target_city": "Vancouver",
        "target_state": "WA",
        "network_domain": "vancouver.aiyoda.com",
    }, {
        "target_city": "Portland",
        "target_state": "OR",
        "network_domain": "portland.aiyoda.com",
    }]

def generate_slug(name):
    """Replicates the Rust compiler slug generation logic in Python."""
    slug = name.lower()
    slug = re.sub(r'[^a-z0-9]+', '-', slug)
    slug = '-'.join([s for s in slug.split('-') if s])
    return slug

def get_nice_category(category):
    """Translates database category slugs to reader-friendly professional terms."""
    mapping = {
        "plumbing": "plumbing",
        "electrical": "electrical",
        "hvac": "HVAC",
        "kitchen_remodel": "kitchen remodeling",
        "septic_system": "septic system",
        "pest_control": "pest control",
    }
    return mapping.get(category, category)

def main():
    db_url = os.environ.get("DATABASE_URL", "postgres://postgres:password@localhost:5432/postgres")
    
    markets = load_markets_config()
    markets_by_city = {m["target_city"].lower(): m for m in markets}
    
    print(f"[Outreach Engine] Connecting to PostgreSQL database...")
    try:
        conn = psycopg2.connect(db_url)
        cursor = conn.cursor()
    except Exception as e:
        print(f"[Outreach Engine Error] Failed to connect to PostgreSQL: {e}")
        return
    
    try:
        cursor.execute("SELECT id, legal_entity_name, service_category, enriched_data, target_city FROM contractors")
        rows = cursor.fetchall()
    except Exception as e:
        print(f"[Outreach Engine Error] Failed to read contractors table: {e}")
        conn.close()
        return

    drafts = []
    
    for row in rows:
        contractor_id, name, service_category, enriched_data_str, target_city = row
        
        email = None
        claimed = False
        
        if enriched_data_str:
            try:
                if isinstance(enriched_data_str, dict):
                    enriched = enriched_data_str
                else:
                    enriched = json.loads(enriched_data_str)
                email = enriched.get("email")
                claimed = enriched.get("claimed", False)
            except Exception as e:
                print(f"[Outreach Engine Warning] Failed to parse enriched_data JSON: {e}")
                pass
                
        # Validate email exists and is not empty, and claimed status is False
        if not email or not isinstance(email, str) or not email.strip():
            continue
            
        if claimed:
            continue
            
        email = email.strip()
        nice_category = get_nice_category(service_category)
        slug = generate_slug(name)
        
        # Look up config details based on target_city
        city_lower = target_city.lower() if target_city else "vancouver"
        m_config = markets_by_city.get(city_lower, {})
        city = m_config.get("target_city", target_city or "Vancouver")
        domain = m_config.get("network_domain", f"{city.lower()}.aiyoda.com")
        
        # Highlight verification link matching compiler.rs exactly in dist/
        city_slug = generate_slug(city)
        niche_slug = generate_slug(service_category)
        profile_url = f"https://{domain}/{city_slug}/{niche_slug}/{slug}/"
        
        subject = f"Your new {nice_category} profile on the {city} Local Directory"
        
        body = (
            f"Hi {name} team,\n\n"
            f"We recently upgraded our {city} professional directory and featured your business in the {nice_category} section based on your local metrics.\n\n"
            f"We have generated a live verification link for your business profile. You can view it and claim ownership here:\n"
            f"Verification Link: {profile_url}\n\n"
            f"Claiming your listing is free and allows you to update your contact info and receive direct leads.\n\n"
            f"Best,\n"
            f"The {city} Directory Team"
        )
        
        drafts.append({
            "to": email,
            "subject": subject,
            "body": body
        })

    dry_run_path = os.path.join(SCRIPT_DIR, "outreach_dry_run.txt")
    try:
        with open(dry_run_path, "w", encoding="utf-8") as f:
            for i, draft in enumerate(drafts, 1):
                f.write(f"=== Draft #{i} ===\n")
                f.write(f"To: {draft['to']}\n")
                f.write(f"Subject: {draft['subject']}\n")
                f.write(f"Body:\n{draft['body']}\n")
                f.write("=" * 40 + "\n\n")
        
        print(f"\n[Outreach Engine Success] Successfully drafted {len(drafts)} outreach emails.")
        print(f"[Outreach Engine Success] Generated outreach logs saved to: {dry_run_path}")
    except Exception as e:
        print(f"[Outreach Engine Error] Failed to write dry run file: {e}")
        
    conn.close()

if __name__ == "__main__":
    main()
