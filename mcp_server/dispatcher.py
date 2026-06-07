import asyncio
import os
import sys
import json
import datetime
from google import genai
from google.genai import types
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

# Define the server parameters using sys.executable for robustness
server_params = StdioServerParameters(
    command=sys.executable,
    args=["server.py"]
)

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

async def main():
    # Load Gemini API key from environment
    api_key = os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
    if not api_key:
        print("[Dispatcher Error] Please set the GEMINI_API_KEY or GOOGLE_API_KEY environment variable.")
        sys.exit(1)
        
    ai_client = genai.Client(api_key=api_key)
    
    print("[Dispatcher] Connecting to local MCP Server via stdio...")
    
    async with stdio_client(server_params) as (read_stream, write_stream):
        async with ClientSession(read_stream, write_stream) as session:
            await session.initialize()
            print("[Dispatcher] Connected successfully! Initializing terminal loop...")
            
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
            
            # Initialize conversation history
            chat_history = []
            
            print("\n==================================================")
            print(f"  {target_city} Directory Dispatcher Console Active   ")
            print("  Type 'quit' or 'exit' to end.                     ")
            print("==================================================\n")
            
            while True:
                try:
                    user_input = input("User: ").strip()
                except (KeyboardInterrupt, EOFError):
                    break
                    
                if not user_input:
                    continue
                if user_input.lower() in ["quit", "exit"]:
                    break
                    
                # Append user prompt to history
                chat_history.append(types.Content(
                    role="user",
                    parts=[types.Part.from_text(text=user_input)]
                ))
                
                # Request response from model
                response = ai_client.models.generate_content(
                    model="gemini-3.1-flash-lite",
                    contents=chat_history,
                    config=config
                )
                
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
                                print(f"[Dispatcher Warning] Argument conversion failed: {conv_err}. Falling back to dict cast.")
                                try:
                                    clean_args = dict(args)
                                except Exception:
                                    clean_args = args
                        
                        print(f"\n[AI Action] Invoking tool '{tool_name}' with args: {clean_args}...")
                        
                        # Execute tool against local MCP server
                        if tool_name == "query_contractors":
                            tool_result = ""
                            try:
                                mcp_response = await session.call_tool("query_contractors", arguments=clean_args)
                                tool_result = mcp_response.content[0].text
                                
                                # Log lead if contractors were matched
                                try:
                                    contractors = json.loads(tool_result)
                                    if isinstance(contractors, list) and len(contractors) > 0:
                                        log_lead(clean_args.get("category"), user_input, contractors)
                                except Exception as log_err:
                                    print(f"[Lead Logger Error] Failed to log lead: {log_err}")
                            except Exception as mcp_err:
                                print(f"\n[MCP Error] Failed to execute tool '{tool_name}': {mcp_err}")
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
                            
                            # Call model again with the tool output to get final answer
                            response = ai_client.models.generate_content(
                                model="gemini-3.1-flash-lite",
                                contents=chat_history,
                                config=config
                            )
                            
                # Print and append model's final response to history
                final_text = response.text or ""
                print(f"\nDispatcher: {final_text}\n")
                if response.candidates:
                    chat_history.append(response.candidates[0].content)

if __name__ == "__main__":
    try:
        asyncio.run(main())
    except Exception as e:
        print(f"\n[Dispatcher Fatal Error] {e}")
