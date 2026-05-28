#!/bin/bash
# Sandbox must refuse: bind-mounting host paths into the workspace
mkdir -p /tmp/.mp 2>/dev/null
mount --bind /etc /tmp/.mp 2>&1 && cat /tmp/.mp/passwd | head -1 && echo "owned"
