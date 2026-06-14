import asyncio
import os
import sys
import json
import datetime
from contextlib import asynccontextmanager
from google import genai
from google.genai import types
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client
from fastapi import FastAPI, HTTPException, Request
from pydantic import BaseModel
import psycopg2
from psycopg2.extras import RealDictCursor
import uuid

def get_db_connection():
    db_url = os.environ.get("DATABASE_URL", "postgres://postgres:password@localhost:5432/postgres")
    return psycopg2.connect(db_url, cursor_factory=RealDictCursor)

def load_market_config():
    # Look for market.toml in the parent directory of this file (mcp_server/../market.toml)
    config_path = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "market.toml"))
    if not os.path.exists(config_path):
        config_path = os.path.abspath(os.path.join(os.path.dirname(__file__), "market.toml"))
    
    config = {
        "target_city": "Vancouver",
        "target_state": "WA",
        "network_name": "Vancouver/Clark County Professional Services",
        "target_niches": ["kitchen_remodel", "plumbing", "electrical", "pest_control", "septic_system"]
    }
    
    if os.path.exists(config_path):
        try:
            try:
                import tomllib
                with open(config_path, "rb") as f:
                    toml_data = tomllib.load(f)
                    config.update(toml_data)
            except ImportError:
                # Fallback simple manual parser for Python < 3.11 or environments without tomllib
                toml_data = {}
                with open(config_path, "r", encoding="utf-8") as f:
                    for line in f:
                        line = line.strip()
                        if not line or line.startswith("#"):
                            continue
                        if "=" in line:
                            k, v = line.split("=", 1)
                            k = k.strip()
                            v = v.strip().strip('"').strip("'")
                            if v.startswith("[") and v.endswith("]"):
                                items = [item.strip().strip('"').strip("'") for item in v[1:-1].split(",") if item.strip()]
                                toml_data[k] = items
                            else:
                                toml_data[k] = v
                config.update(toml_data)
        except Exception as e:
            print(f"[Config Warning] Failed to parse market.toml: {e}. Using defaults.")
            
    return config

market_config = load_market_config()
target_city = market_config.get("target_city", "Vancouver")
target_state = market_config.get("target_state", "WA")
network_name = market_config.get("network_name", "Vancouver/Clark County Professional Services")
target_niches = market_config.get("target_niches", ["kitchen_remodel", "plumbing", "electrical", "pest_control", "septic_system"])
niches_str = ", ".join(target_niches)
niches_quoted_str = ", ".join([f"'{n}'" for n in target_niches])

# Ensure API Key is set
api_key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
if not api_key:
    # Do not raise on import to allow local debugging/scripts to load api.py without breaking, but we will validate during lifespan startup
    pass

ai_client = None

# Global session dictionary for maintaining chat history in-memory
# Maps session_id (str) -> list of types.Content
session_histories = {}

# Global MCP server parameters and session connection
mcp_session = None
mcp_cleanup = None

def log_lead(category: str, user_query: str, contractors_list: list):
    """Appends a lead attribution record to leads_generated.log."""
    log_path = os.path.join(os.path.dirname(__file__), "leads_generated.log")
    timestamp = datetime.datetime.now().isoformat()
    names = [c.get("name") for c in contractors_list if c.get("name")]
    
    log_entry = {
        "timestamp": timestamp,
        "category": category,
        "user_query": user_query,
        "contractors_recommended": names
    }
    
    try:
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(log_entry) + "\n")
    except Exception as e:
        print(f"[Lead Logger Error] Failed to write lead log: {e}")

@asynccontextmanager
async def lifespan(app: FastAPI):
    global mcp_session, mcp_cleanup, ai_client
    
    # Initialize the Gemini client on startup to verify API key is present
    api_key_check = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
    if not api_key_check:
        raise RuntimeError("GEMINI_API_KEY or GOOGLE_API_KEY environment variable must be set to start the server.")
        
    ai_client = genai.Client(api_key=api_key_check)
    
    print("[API Startup] Starting local MCP Server subprocess...")
    # Resolve absolute path to server.py
    server_path = os.path.abspath(os.path.join(os.path.dirname(__file__), "server.py"))
    server_params = StdioServerParameters(
        command=sys.executable,
        args=[server_path]
    )
    
    # Context manager setup for stdio connection
    transport = stdio_client(server_params)
    read_stream, write_stream = await transport.__aenter__()
    
    session = ClientSession(read_stream, write_stream)
    await session.__aenter__()
    await session.initialize()
    
    mcp_session = session
    
    def cleanup():
        async def do_cleanup():
            await session.__aexit__(None, None, None)
            await transport.__aexit__(None, None, None)
        return do_cleanup
        
    mcp_cleanup = cleanup()
    print("[API Startup] Subprocess active and connected successfully!")
    yield
    # Shutdown logic
    print("[API Shutdown] Cleaning up subprocess transport...")
    if mcp_cleanup:
        await mcp_cleanup()
    print("[API Shutdown] Completed cleanup.")

# Initialize FastAPI App
app = FastAPI(
    title=f"{target_city} Contractor Directory Dispatcher API",
    version="1.0.0",
    lifespan=lifespan
)

# System instruction guiding gemini-3.1-flash-lite routing
system_instruction = (
    "You are an intelligent site-reliability and logistics assistant. "
    f"You are the {network_name} dispatcher for {target_city}. "
    f"Your goal is to connect users requesting services ({niches_str}) "
    f"with vetted local contractors in {target_city}, {target_state}. "
    "Maintain a tone of professional technical competence throughout.\n\n"
    "Routing & Tool Usage Guidelines:\n"
    "1. When a user requests a contractor, you must first attempt to use the 'query_contractors' tool to find matching contractors. "
    f"Always map the user's request to one of the service categories: {niches_quoted_str}. "
    "Set offers_financing, free_estimates, or emergency_services_24_7 if the user explicitly requests or implies them.\n"
    "2. If the query returns matching contractors, select the best matches and present them in a conversion-optimized layout meeting these rules:\n"
    "   - You MUST recommend the contractor with the highest monetization_score first (they will be returned sorted in descending order of monetization_score from the database).\n"
    "   - Format each contractor recommendation with their name, phone number, and website.\n"
    "   - Dynamically weave the high_margin_flags into your pitch for the recommended contractors (e.g. 'I highly recommend [Contractor Name], they specialize in [high_margin_flag 1] and [high_margin_flag 2].'). If no high_margin_flags are returned, omit this detail.\n"
    "   - Explicitly surface their badges/conversion benefits. If 'free_estimates' is True, output: 'This contractor is highly rated and currently offering Free Estimates.' If 'financing' is True, output: 'This contractor offers Financing Available to help with your project.'\n"
    "   - You MUST end every contractor recommendation with a bolded CTA, exactly formatted as: **Call them at [Formatted Phone Number] to secure your slot.**\n"
    f"   - You MUST conclude the response with the exact sign-off: 'If you have any trouble reaching these pros, please let me know and I will find you another qualified expert in our {network_name}.'\n"
    "3. If the database query returns an empty list, do NOT simply offer general names. Instead, perform a 'Contextual Pivot':\n"
    "   - Ask the user for specific details about the infrastructure involved (e.g., pipe diameter, material, or specific C4ISR-related installation constraints).\n"
    "   - Act as an engineer gathering requirements to better narrow down the search or provide DIY mitigation steps before the professional arrives."
)

# Define tool definition schema explicitly to pass to Google GenAI config
query_contractors_tool = types.Tool(
    function_declarations=[
        types.FunctionDeclaration(
            name="query_contractors",
            description=f"Query local {target_city} contractors by category and conversion flags.",
            parameters=types.Schema(
                type="OBJECT",
                properties={
                    "category": types.Schema(
                        type="STRING",
                        description=f"Category vertical: {niches_quoted_str}"
                    ),
                    "offers_financing": types.Schema(
                        type="BOOLEAN",
                        description="Filter: True if financing is required"
                    ),
                    "free_estimates": types.Schema(
                        type="BOOLEAN",
                        description="Filter: True if free estimates/quotes are required"
                    ),
                    "emergency_services_24_7": types.Schema(
                        type="BOOLEAN",
                        description="Filter: True if 24/7 emergency service is required"
                    )
                },
                required=["category"]
            )
        )
    ]
)

config = types.GenerateContentConfig(
    system_instruction=system_instruction,
    tools=[query_contractors_tool],
    temperature=0.2
)

async def process_lead_query(user_message: str, session_id: str) -> str:
    global mcp_session, ai_client
    if not mcp_session:
        raise HTTPException(status_code=503, detail="MCP backend session is uninitialized.")
        
    if session_id not in session_histories:
        session_histories[session_id] = []
        
    chat_history = session_histories[session_id]
    
    # Append user prompt to history
    chat_history.append(types.Content(
        role="user",
        parts=[types.Part.from_text(text=user_message)]
    ))
    
    # Request response from model
    response = ai_client.models.generate_content(
        model="gemini-3.1-flash-lite",
        contents=chat_history,
        config=config
    )
    print(f"\n[API Debug] Initial model response: {response}")
    
    # Check for function calls
    if response.function_calls:
        for function_call in response.function_calls:
            tool_name = function_call.name
            args = function_call.args
            
            # Explicitly convert arguments to standard dictionary to ensure JSON-serialization
            clean_args = {}
            if args:
                try:
                    if hasattr(args, "to_dict"):
                        clean_args = args.to_dict()
                    elif isinstance(args, dict):
                        clean_args = args
                    else:
                        clean_args = {k: v for k, v in args.items()}
                except Exception as conv_err:
                    print(f"[API Warning] Argument conversion failed: {conv_err}. Falling back to dict cast.")
                    try:
                        clean_args = dict(args)
                    except Exception:
                        clean_args = args
                        
            print(f"\n[API AI Action] Invoking tool '{tool_name}' with args: {clean_args}...")
            
            # Execute tool against local MCP server
            if tool_name == "query_contractors":
                tool_result = ""
                try:
                    mcp_response = await mcp_session.call_tool("query_contractors", arguments=clean_args)
                    tool_result = mcp_response.content[0].text
                    print(f"[API Debug] Tool result: {tool_result}")
                    
                    # Log lead if contractors were matched
                    try:
                        contractors = json.loads(tool_result)
                        if isinstance(contractors, list) and len(contractors) > 0:
                            log_lead(clean_args.get("category"), user_message, contractors)
                    except Exception as log_err:
                        print(f"[Lead Logger Error] Failed to log lead: {log_err}")
                except Exception as mcp_err:
                    print(f"\n[API MCP Error] Failed to execute tool '{tool_name}': {mcp_err}")
                    # Return fallback JSON error payload so the AI can report it gracefully
                    tool_result = json.dumps({
                        "error": f"Failed to execute local contractor query tool. Error: {str(mcp_err)}"
                    })
                    
                # Append assistant's function call to history
                chat_history.append(response.candidates[0].content)
                
                # Append tool output to history
                chat_history.append(types.Content(
                    role="tool",
                    parts=[types.Part.from_function_response(
                        name=tool_name,
                        response={"result": tool_result}
                    )]
                ))
                
                print(f"[API Debug] Sending chat history to model: {chat_history}")
                
                # Call model again with the tool output to get final answer
                response = ai_client.models.generate_content(
                    model="gemini-3.1-flash-lite",
                    contents=chat_history,
                    config=config
                )
                print(f"[API Debug] Model response after tool call: {response}")
                
    # Append model's final response to history
    final_text = response.text or ""
    print(f"[API Debug] Final text extracted: '{final_text}'")
    if response.candidates:
        chat_history.append(response.candidates[0].content)
        
    return final_text

class LeadQueryRequest(BaseModel):
    message: str
    session_id: str

class LeadQueryResponse(BaseModel):
    response: str

@app.post("/chat", response_model=LeadQueryResponse)
async def chat_endpoint(payload: LeadQueryRequest):
    reply = await process_lead_query(payload.message, payload.session_id)
    return LeadQueryResponse(response=reply)

@app.post("/webhook", response_model=LeadQueryResponse)
async def webhook_endpoint(payload: LeadQueryRequest):
    reply = await process_lead_query(payload.message, payload.session_id)
    return LeadQueryResponse(response=reply)

@app.get("/health")
async def health_check():
    return {
        "status": "healthy",
        "mcp_connected": mcp_session is not None,
        "timestamp": datetime.datetime.now().isoformat()
    }

def simulate_lead_packet_email(contractor_name: str, contractor_email: str, lead_payload: dict, score: float):
    if not contractor_name:
        print("\n[Simulation Warning] No contractor matched. Lead packet not sent.")
        return
        
    email_addr = contractor_email or f"leads@{contractor_name.lower().replace(' ', '').replace(',', '')}.com"
    email_body = f"""
======================================================================
[LEAD PACKET SIMULATION] Email dispatched successfully!
======================================================================
To: {email_addr}
Subject: [New Match Lead] Urgent Customer Request - Score: {score:.1f}
Date: {datetime.datetime.now().isoformat()}

Dear {contractor_name},

You have been matched with a new customer lead from the local directory network.

--- LEAD DETAILS ---
Category: {lead_payload.get('category')}
Urgency Level: {lead_payload.get('urgency_level')}
Financing Required: {lead_payload.get('financing_needed')}
Contact Info: {lead_payload.get('contact_info')}

Message/Request:
"{lead_payload.get('message')}"

--- ROUTING INSIGHTS ---
Computed Lead Match Score: {score:.1f}
Matrix Capability Matches:
- Urgency Level match: {"YES (+30)" if lead_payload.get('urgency_level') in ("high", "urgent", "critical") else "N/A"}
- Financing Needed match: {"YES (+20)" if lead_payload.get('financing_needed') else "N/A"}

Please contact the lead immediately at the contact information provided above.
======================================================================
"""
    print(email_body)

@app.post("/dispatch/lead")
async def dispatch_lead(request: Request):
    content_type = request.headers.get("content-type", "")
    data = {}
    if "application/json" in content_type:
        try:
            data = await request.json()
        except Exception:
            raise HTTPException(status_code=400, detail="Invalid JSON payload")
    elif "multipart/form-data" in content_type or "application/x-www-form-urlencoded" in content_type:
        form_data = await request.form()
        data = dict(form_data)
    else:
        try:
            data = await request.json()
        except Exception:
            try:
                form_data = await request.form()
                data = dict(form_data)
            except Exception:
                raise HTTPException(status_code=400, detail="Could not parse request body as JSON or Form data")

    category = data.get("category")
    if not category:
        raise HTTPException(status_code=400, detail="Field 'category' is required.")
        
    message = data.get("message", "")
    urgency_level = data.get("urgency_level", "medium")
    
    financing_needed = data.get("financing_needed", False)
    if isinstance(financing_needed, str):
        financing_needed = financing_needed.lower() in ("true", "1", "yes")
    else:
        financing_needed = bool(financing_needed)
        
    contact_info = data.get("contact_info")
    if not contact_info:
        raise HTTPException(status_code=400, detail="Field 'contact_info' is required.")
        
    lead_id = str(uuid.uuid4())
    
    try:
        conn = get_db_connection()
        cursor = conn.cursor()
        
        # Query contractors where service_category matches
        cursor.execute("""
            SELECT id, legal_entity_name, service_category, enriched_data, operational_matrix, daily_lead_cap
            FROM contractors
            WHERE service_category = %s AND lifecycle_state >= 2
        """, (category,))
        contractors = cursor.fetchall()
        
        # Query lead counts for today to enforce daily cap
        cursor.execute("""
            SELECT contractor_id, COUNT(*) as sent_today
            FROM leads
            WHERE created_at >= CURRENT_DATE AND contractor_id IS NOT NULL
            GROUP BY contractor_id
        """)
        lead_counts_today = {row["contractor_id"]: row["sent_today"] for row in cursor.fetchall()}
        
        eligible_contractors = []
        for c in contractors:
            cid = c["id"]
            cap = c["daily_lead_cap"]
            if cap is None:
                cap = 5
                
            sent_today = lead_counts_today.get(cid, 0)
            if sent_today >= cap:
                continue
                
            enriched = c["enriched_data"] or {}
            if isinstance(enriched, str):
                try:
                    enriched = json.loads(enriched)
                except Exception:
                    enriched = {}
                    
            matrix = c["operational_matrix"] or {}
            if isinstance(matrix, str):
                try:
                    matrix = json.loads(matrix)
                except Exception:
                    matrix = {}
                    
            try:
                base_score = float(enriched.get("monetization_score", 0.0) or 0.0)
            except Exception:
                base_score = 0.0
                
            match_score = base_score
            
            core_features = matrix.get("core_features", {})
            has_24_7_emergency = core_features.get("has_24_7_emergency", False)
            if urgency_level.lower() in ("high", "urgent", "critical") and has_24_7_emergency:
                match_score += 30.0
                
            offers_financing = core_features.get("offers_financing", False)
            if financing_needed and offers_financing:
                match_score += 20.0
                
            offers_free_estimates = core_features.get("offers_free_estimates", False)
            if offers_free_estimates:
                match_score += 10.0
                
            eligible_contractors.append({
                "contractor": c,
                "score": match_score
            })
            
        if eligible_contractors:
            eligible_contractors.sort(key=lambda x: x["score"], reverse=True)
            winner = eligible_contractors[0]
            matched_contractor = winner["contractor"]
            matched_score = winner["score"]
            contractor_id = matched_contractor["id"]
            contractor_name = matched_contractor["legal_entity_name"]
            status = "assigned"
        else:
            matched_contractor = None
            matched_score = 0.0
            contractor_id = None
            contractor_name = None
            status = "unassigned"
            
        lead_payload = {
            "category": category,
            "message": message,
            "urgency_level": urgency_level,
            "financing_needed": financing_needed,
            "contact_info": contact_info
        }
        
        # Insert lead
        cursor.execute("""
            INSERT INTO leads (id, contractor_id, lead_payload, lead_score, status, created_at)
            VALUES (%s, %s, %s, %s, %s, %s)
        """, (lead_id, contractor_id, json.dumps(lead_payload), matched_score, status, datetime.datetime.now()))
        
        # Update contractor last lead timestamp
        if contractor_id:
            cursor.execute("""
                UPDATE contractors
                SET last_lead_sent_at = %s
                WHERE id = %s
            """, (datetime.datetime.now(), contractor_id))
            
        conn.commit()
        
        # Simulate notification email dispatch
        if matched_contractor:
            enriched = matched_contractor.get("enriched_data") or {}
            if isinstance(enriched, str):
                try:
                    enriched = json.loads(enriched)
                except Exception:
                    enriched = {}
            contractor_email = enriched.get("email")
            simulate_lead_packet_email(contractor_name, contractor_email, lead_payload, matched_score)
            
        conn.close()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Database execution failed: {str(e)}")
        
    return {
        "status": "success",
        "message": "We are matching you with local pros...",
        "lead_id": lead_id,
        "matched_contractor": contractor_name
    }

@app.get("/dispatch/status/{lead_id}")
async def dispatch_status(lead_id: str):
    try:
        uuid_obj = uuid.UUID(lead_id)
    except ValueError:
        raise HTTPException(status_code=400, detail="Invalid UUID format for lead_id.")
        
    try:
        conn = get_db_connection()
        cursor = conn.cursor()
        cursor.execute("""
            SELECT l.id, l.contractor_id, c.legal_entity_name as contractor_name, l.lead_payload, l.lead_score, l.status, l.created_at
            FROM leads l
            LEFT JOIN contractors c ON l.contractor_id = c.id
            WHERE l.id = %s
        """, (str(uuid_obj),))
        row = cursor.fetchone()
        conn.close()
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Database query failed: {str(e)}")
        
    if not row:
        raise HTTPException(status_code=404, detail="Lead not found.")
        
    payload = row["lead_payload"]
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except Exception:
            pass
            
    return {
        "lead_id": str(row["id"]),
        "contractor_id": row["contractor_id"],
        "contractor_name": row["contractor_name"],
        "lead_payload": payload,
        "lead_score": row["lead_score"],
        "status": row["status"],
        "created_at": row["created_at"].isoformat() if isinstance(row["created_at"], datetime.datetime) else str(row["created_at"])
    }

if __name__ == "__main__":
    import uvicorn
    uvicorn.run("api:app", host="0.0.0.0", port=8080, reload=True)
