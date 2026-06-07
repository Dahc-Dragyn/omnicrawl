# Use a slim Python 3.11 base image
FROM python:3.11-slim

# Set environment variables for Python performance & safety
ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PORT=8080

# Create working directory
WORKDIR /app

# Install minimal compiler dependencies for compiling python packages if needed
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# Copy requirements manifest and install dependencies
COPY mcp_server/requirements.txt ./mcp_server/requirements.txt
RUN pip install --no-cache-dir -r ./mcp_server/requirements.txt

# Copy Python servers and target database matching workspace directory structure
COPY mcp_server/server.py ./mcp_server/server.py
COPY mcp_server/api.py ./mcp_server/api.py
COPY target_bravo/target_bravo.db ./target_bravo/target_bravo.db

# Expose standard Cloud Run port (8080)
EXPOSE 8080

# Execute Uvicorn web server
CMD ["uvicorn", "mcp_server.api:app", "--host", "0.0.0.0", "--port", "8080"]
