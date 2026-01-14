# Personal Jesus - Rust Edition 🎵

*Lift up the receiver, I'll make you a believer* 🎵

A Rust implementation of a web app to interact with the Mistral 7B AI model, now with Docker support so anyone can run it without installing models locally!

![Demo Screenshot](./personaljsesusdemo.png)

## Features

- 🦀 **Rust Backend**: Fast and efficient server using Actix-web
- 🐳 **Docker Compose**: Complete setup with Ollama and model included
- 🔄 **Streaming Responses**: Real-time AI responses as they're generated
- 🎨 **Clean UI**: Simple, retro-styled chat interface
- 🚀 **Easy Setup**: One command to get everything running

## Quick Start

### Using Docker Compose (Recommended)

This is the easiest way to run the project - Docker will automatically pull and set up the model for you!

1. **Prerequisites**:
   - [Docker](https://docs.docker.com/get-docker/)
   - [Docker Compose](https://docs.docker.com/compose/install/)

2. **Run the application**:
   ```bash
   docker-compose up
   ```

   The first time you run this, it will:
   - Pull the Ollama image
   - Pull the Mistral 7B model (~4.1GB)
   - Build the Rust application
   - Start everything up

3. **Access the app**:
   Open your browser to [http://localhost:8080](http://localhost:8080)

4. **Stop the application**:
   ```bash
   docker-compose down
   ```

   To also remove the model data:
   ```bash
   docker-compose down -v
   ```

### Local Development (Without Docker)

If you want to run it locally for development:

1. **Prerequisites**:
   - [Rust](https://rustup.rs/) (1.75 or newer)
   - [Ollama](https://ollama.ai/) installed locally

2. **Pull the model**:
   ```bash
   ollama pull mistral:7b
   ```

3. **Set environment variables**:
   ```bash
   export OLLAMA_URL=http://localhost:11434
   export MODEL_NAME=mistral:7b
   export PORT=8080
   ```

4. **Run the application**:
   ```bash
   cargo run --release
   ```

5. **Access the app**:
   Open your browser to [http://localhost:8080](http://localhost:8080)

## Architecture

```
┌─────────────────┐
│   Web Browser   │
│  (index.html)   │
└────────┬────────┘
         │ HTTP
         ▼
┌─────────────────┐
│   Rust Server   │
│  (Actix-web)    │
└────────┬────────┘
         │ HTTP
         ▼
┌─────────────────┐
│     Ollama      │
│  (AI Backend)   │
└─────────────────┘
```

## Project Structure

```
personal-jesus-rust/
├── src/
│   └── main.rs           # Rust server implementation
├── static/
│   └── index.html        # Frontend HTML/CSS/JS
├── Dockerfile            # Container definition for Rust app
├── docker-compose.yml    # Multi-container orchestration
├── Cargo.toml            # Rust dependencies
└── README.md            # This file
```

## Configuration

Environment variables you can customize:

- `PORT`: Server port (default: 8080)
- `OLLAMA_URL`: Ollama API URL (default: http://ollama:11434)
- `MODEL_NAME`: Model to use (default: mistral:7b)
- `RUST_LOG`: Logging level (default: info)

## API Endpoints

- `POST /api/chat`: Send a message and receive streaming response
  ```json
  {
    "message": "Your question here"
  }
  ```

- `GET /health`: Health check endpoint

## Troubleshooting

### Docker Conflicts with Other Containers

**This setup won't conflict with your other Docker containers!** Each container has a unique name and uses isolated networks. However, there are a few things to be aware of:

**Port conflicts:**
If you're already using port 8080 or 11434, you'll need to change the ports:

```yaml
# In docker-compose.yml, change:
services:
  ollama:
    ports:
      - "11435:11434"  # Change 11434 to 11435 (or any free port)
  
  web:
    ports:
      - "3000:8080"    # Change 8080 to 3000 (or any free port)
```

Then visit `http://localhost:3000` instead.

**Volume conflicts:**
The volumes are uniquely named (`ollama_data`) so they won't interfere with other containers.

**Network conflicts:**
This setup uses Docker's default bridge network, so it won't conflict with custom networks from other projects.

**Resource conflicts:**
Mistral 7B needs ~8GB RAM. If you're running memory-intensive containers, you might need to stop them first or increase Docker's memory limit in Docker Desktop settings.

### Docker Issues

**Port already in use:**
```bash
# Change the port in docker-compose.yml
ports:
  - "3000:8080"  # Use port 3000 instead
```

**Model download is slow:**
The Mistral 7B model is about 4.1GB. The first run will take several minutes to download.

**Out of memory:**
Make sure Docker has enough memory allocated (at least 4GB recommended).

### Local Development Issues

**Ollama not running:**
```bash
# Start Ollama service
ollama serve
```

**Connection refused:**
Make sure Ollama is running and accessible at the configured URL.

## Comparison with Original Next.js Version

| Feature | Next.js Version | Rust Version |
|---------|----------------|--------------|
| Language | JavaScript | Rust |
| Framework | Next.js | Actix-web |
| Performance | Good | Excellent |
| Memory Usage | ~150MB | ~10MB |
| Docker Setup | Manual | Automated |
| Startup Time | ~3s | ~0.1s |

## Contributing

Feel free to open issues or submit pull requests!

## License

MIT

## Credits

- Original Next.js version by branchwag
- Built with [Actix-web](https://actix.rs/)
- Powered by [Ollama](https://ollama.ai/) and [Mistral AI](https://mistral.ai/)
