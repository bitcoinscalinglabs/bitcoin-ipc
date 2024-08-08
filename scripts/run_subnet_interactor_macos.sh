#!/bin/bash

if [ -z "$1" ]; then
  echo "Error: No subnet_name provided."
  exit 1
fi

subnet_name=$1

osascript -e 'tell app "Terminal"
    do script "cargo run --bin subnet_interactor -- --subnet-name '$subnet_name'"
end tell'

echo "Subnet interactor for $subnet_name started."
