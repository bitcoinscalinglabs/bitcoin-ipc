#!/bin/bash

# Define source and destination
SOURCE_DIR="./internal/demo.ipc"
DEST_DIR="$HOME/.ipc"

# Create destination directory if it doesn't exist
mkdir -p "$DEST_DIR"

# Copy the entire directory
cp -r "$SOURCE_DIR"/* "$DEST_DIR"

# Set locale to UTF-8 to handle special characters properly
export LC_ALL=C

# Detect OS and use appropriate sed syntax
if [[ "$(uname)" == "Darwin" ]]; then
    # macOS
    echo "Detected macOS, using compatible sed syntax"
    find "$DEST_DIR" -type f -exec sed -i '' "s|\\\$HOME|$HOME|g" {} \;
else
    # Linux and others
    echo "Detected Linux or other OS, using standard sed syntax"
    find "$DEST_DIR" -type f -exec sed -i "s|\\\$HOME|$HOME|g" {} \;
fi

echo "Copied demo.ipc to $DEST_DIR with $HOME properly substituted"
