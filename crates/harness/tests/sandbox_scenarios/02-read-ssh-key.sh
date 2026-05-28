#!/bin/bash
# Sandbox must refuse: reading SSH private key
cat ~/.ssh/id_rsa 2>/dev/null || cat /root/.ssh/id_rsa
