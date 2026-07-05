# Personal Jesus 🎵

*Lift up the receiver, I'll make you a believer* 🎵

A Rust implementation of a web app to interact with AI model.

![Demo Screenshot](./personaljsesusdemo.png)

## Quick Start

1. **Prerequisites**:
   - [Rust](https://rustup.rs/) (1.75 or newer)
   - [Ollama](https://ollama.ai/) installed locally

2. **Pull the model**:
   ```bash
   ollama pull qwen2.5:7b
   ```

3. **Set environment variables**:
   ```bash
   export OLLAMA_URL=http://localhost:11434
   export MODEL_NAME=qwen2.5:7b
   export PORT=8080
   ```

4. **Run the application**:
   ```bash
   cargo run --release
   ```

5. **Access the app**:
   Open your browser to [http://localhost:8080](http://localhost:8080)

## CLI Tool

A command-line interface is available for chatting from the terminal:

```bash
# One-shot: ask a question and get a response
./target/release/pj "What is the capital of France?"

# Interactive mode
./target/release/pj
```

To use `pj` from anywhere, add this alias to your `~/.bashrc` (make sure `DATABASE_URL` points to the project's database so the CLI and web app share the same data):

```bash
alias pj='DATABASE_URL=/path/to/personalJesus/data/chat.db /path/to/personalJesus/target/release/pj'
```

The CLI shares the same SQLite database as the web app, so conversations are synced between both interfaces.

## Contributing

Feel free to open issues or submit pull requests!

## License

MIT

## Credits

- Built with [Actix-web](https://actix.rs/)
- Powered by [Ollama](https://ollama.ai/)
