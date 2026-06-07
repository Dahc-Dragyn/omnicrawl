#!/usr/bin/env python3
"""
Omnicrawl Outreach Engine v2
Queries the target_bravo PostgreSQL database for contractors with valid, unclaimed emails,
generates a custom Window Sticker QR Code pointing to their profile, renders personalized B2B
emails via Jinja2 offline templates using operational_matrix data, and updates tracking columns in DB.
"""

import os
import re
import json
import psycopg2
from jinja2 import Template
import qrcode

# Define directories relative to this script
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))

# Jinja2 Email Template
EMAIL_TEMPLATE = """Subject: Your new {{ category }} profile on the {{ city }} Local Directory

Hi {{ name }} team,

We recently upgraded our {{ city }} professional directory and featured your business in the {{ category }} section based on your local metrics.

We have generated a live verification link for your business profile. You can view it and claim ownership here:
Verification Link: {{ profile_url }}

{% if has_24_7_emergency -%}
As a certified emergency responder in {{ city }}, your profile is configured with our high-visibility "Emergency Lead Capture" widget. This ensures local clients facing urgent plumbing/HVAC/electrical issues can contact you instantly.
{%- elif not offers_financing -%}
Currently, your listing is missing a Financing Badge. We've prepared a "Financing Badge Upgrade" for your profile—offering financing options to clients on their profiles has been shown to help local firms close up to 30% more sales. Let us know if you'd like us to enable this badge for you.
{%- else -%}
Your listing highlights your premium qualifications, licensing status, and verified local presence to help you stand out.
{%- endif %}

We have also generated a custom physical Window Sticker QR code for your business, bridging the offline-to-online gap so your physical customers can instantly scan and view your L&I verified status. (We've saved this locally as {{ qr_code_filename }}).

Claiming your listing is free and allows you to update your contact info and receive direct leads.

Best,
The {{ city }} Directory Team
"""

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
        # Fetch contractors that haven't been contacted yet
        cursor.execute(
            "SELECT id, legal_entity_name, service_category, enriched_data, target_city, operational_matrix "
            "FROM contractors "
            "WHERE outreach_status = 'uncontacted' OR outreach_status IS NULL"
        )
        rows = cursor.fetchall()
    except Exception as e:
        print(f"[Outreach Engine Error] Failed to read contractors table: {e}")
        conn.close()
        return

    print(f"[Outreach Engine] Found {len(rows)} contractors to process.")
    drafts = []
    
    # Compile the Jinja2 template
    template = Template(EMAIL_TEMPLATE)
    
    for row in rows:
        contractor_id, name, service_category, enriched_data_str, target_city, operational_matrix = row
        
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
                
        # Validate email exists, is not empty, and claimed status is False
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
        
        # Extract operational matrix properties safely
        has_24_7_emergency = False
        offers_financing = False
        
        if operational_matrix:
            try:
                if isinstance(operational_matrix, dict):
                    matrix = operational_matrix
                else:
                    matrix = json.loads(operational_matrix)
                core_features = matrix.get("core_features", {})
                has_24_7_emergency = core_features.get("has_24_7_emergency", False)
                offers_financing = core_features.get("offers_financing", False)
            except Exception as e:
                print(f"[Outreach Engine Warning] Failed to parse operational_matrix JSON: {e}")
                pass
        
        # Generate Window Sticker QR Code (PNG)
        qrcodes_dir = os.path.join(SCRIPT_DIR, "outreach_qrcodes")
        os.makedirs(qrcodes_dir, exist_ok=True)
        qr_filename = f"{slug}_qr.png"
        qr_path = os.path.join(qrcodes_dir, qr_filename)
        
        try:
            qr = qrcode.QRCode(version=1, box_size=10, border=4)
            qr.add_data(profile_url)
            qr.make(fit=True)
            img = qr.make_image(fill_color="black", back_color="white")
            img.save(qr_path)
        except Exception as e:
            print(f"[Outreach Engine Warning] Failed to generate QR Code for {name}: {e}")
            qr_filename = "[Error generating QR Code]"

        # Render email body with Jinja2 template
        rendered_body = template.render(
            name=name,
            city=city,
            category=nice_category,
            profile_url=profile_url,
            has_24_7_emergency=has_24_7_emergency,
            offers_financing=offers_financing,
            qr_code_filename=qr_filename
        )
        
        # Parse subject from template body output
        subject = f"Your new {nice_category} profile on the {city} Local Directory"
        body_content = rendered_body
        if rendered_body.startswith("Subject:"):
            lines = rendered_body.split("\n", 1)
            subject = lines[0].replace("Subject:", "").strip()
            body_content = lines[1].strip() if len(lines) > 1 else ""

        drafts.append({
            "to": email,
            "subject": subject,
            "body": body_content
        })
        
        # Update PostgreSQL database to track outreach status
        try:
            cursor.execute(
                "UPDATE contractors "
                "SET outreach_status = 'sent', last_outreached_at = CURRENT_TIMESTAMP "
                "WHERE id = %s",
                (contractor_id,)
            )
        except Exception as e:
            print(f"[Outreach Engine Warning] Failed to update PostgreSQL tracking for {name}: {e}")
            pass

    # Commit the database changes
    try:
        conn.commit()
    except Exception as e:
        print(f"[Outreach Engine Error] Failed to commit Postgres updates: {e}")
        pass
        
    dry_run_path = os.path.join(SCRIPT_DIR, "outreach_dry_run_v2.txt")
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
