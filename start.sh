#!/bin/bash

echo "🎵 Personal Jesus - Rust Edition 🎵"
echo "==================================="
echo ""

# Check if Docker is installed
if ! command -v docker &> /dev/null; then
    echo "❌ Docker is not installed. Please install Docker first:"
    echo "   https://docs.docker.com/get-docker/"
    exit 1
fi

# Check if Docker Compose is installed
if ! command -v docker-compose &> /dev/null && ! docker compose version &> /dev/null; then
    echo "❌ Docker Compose is not installed. Please install Docker Compose first:"
    echo "   https://docs.docker.com/compose/install/"
    exit 1
fi

echo "✅ Docker detected"
echo ""
echo "Starting Personal Jesus..."
echo ""
echo "This will:"
echo "  1. Pull the Ollama image"
echo "  2. Download the Mistral 7B model (~4.1GB)"
echo "  3. Build the Rust application"
echo "  4. Start everything up"
echo ""
echo "The first run may take a few minutes..."
echo ""

# Use docker compose (newer) or docker-compose (older)
if docker compose version &> /dev/null; then
    docker compose up
else
    docker-compose up
fi
