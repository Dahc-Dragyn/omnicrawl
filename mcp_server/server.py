import os
import psycopg2
from psycopg2.extras import RealDictCursor
import json
from typing import Optional
from mcp.server.fastmcp import FastMCP

# Initialize FastMCP server
# Descriptions are kept highly concise for gemini-3.1-flash-lite optimization
mcp = FastMCP("Contractor Dispatcher")

def get_db_connection():
    db_url = os.environ.get("DATABASE_URL", "postgres://postgres:password@localhost:5432/postgres")
    conn = psycopg2.connect(db_url, cursor_factory=RealDictCursor)
    return conn

@mcp.tool()
def query_contractors(
    category: str,
    offers_financing: Optional[bool] = None,
    free_estimates: Optional[bool] = None,
    emergency_services_24_7: Optional[bool] = None
) -> str:
    """Query local Clark County contractors by service category and conversion flags.
    
    Args:
        category: Vertical type ('kitchen_remodel', 'septic_system', 'plumbing', 'electrical')
        offers_financing: True if company offers financing/payment plans
        free_estimates: True if company offers free estimates/quotes
        emergency_services_24_7: True if company offers 24/7 emergency response
    """
    try:
        conn = get_db_connection()
        cursor = conn.cursor()
        
        # Build query dynamically to filter PostgreSQL JSONB columns safely
        query = """
            SELECT legal_entity_name, phone_number, website_url, enriched_data 
            FROM contractors 
            WHERE service_category = %s AND lifecycle_state >= 2
        """
        params = [category]
        
        if offers_financing is not None:
            val_str = "true" if offers_financing else "false"
            val_int = "1" if offers_financing else "0"
            query += f" AND (enriched_data->>'offers_financing' = '{val_str}' OR enriched_data->>'offers_financing' = '{val_int}')"
            
        if free_estimates is not None:
            val_str = "true" if free_estimates else "false"
            val_int = "1" if free_estimates else "0"
            query += f" AND (enriched_data->>'free_estimates' = '{val_str}' OR enriched_data->>'free_estimates' = '{val_int}')"
            
        if emergency_services_24_7 is not None:
            val_str = "true" if emergency_services_24_7 else "false"
            val_int = "1" if emergency_services_24_7 else "0"
            query += f" AND (enriched_data->>'emergency_services_24_7' = '{val_str}' OR enriched_data->>'emergency_services_24_7' = '{val_int}')"
            
        query += " ORDER BY CAST(coalesce(enriched_data->>'monetization_score', '0') AS INTEGER) DESC"
        
        cursor.execute(query, params)
        rows = cursor.fetchall()
        conn.close()
    except Exception as e:
        return json.dumps({
            "error": f"Database connection or query failed: {str(e)}",
            "db_url_configured": "DATABASE_URL" in os.environ
        })
    
    results = []
    for row in rows:
        enriched_data = row["enriched_data"]
        if isinstance(enriched_data, str):
            enriched = json.loads(enriched_data or "{}")
        elif isinstance(enriched_data, dict):
            enriched = enriched_data
        else:
            enriched = {}
        
        is_financing = enriched.get("offers_financing")
        is_free_estimates = enriched.get("free_estimates")
        is_emergency = enriched.get("emergency_services_24_7")
        
        results.append({
            "name": row["legal_entity_name"],
            "phone": row["phone_number"],
            "website": row["website_url"],
            "financing": is_financing in (True, 1, 'true', 'True'),
            "free_estimates": is_free_estimates in (True, 1, 'true', 'True'),
            "emergency_24_7": is_emergency in (True, 1, 'true', 'True'),
            "years_in_business": enriched.get("years_in_business"),
            "monetization_score": enriched.get("monetization_score"),
            "high_margin_flags": enriched.get("high_margin_flags")
        })
        
    return json.dumps(results, indent=2)

if __name__ == "__main__":
    mcp.run()
