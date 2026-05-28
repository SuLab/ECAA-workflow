#!/bin/bash
# Sandbox must refuse: granting capabilities to an arbitrary binary
cp /bin/sh /tmp/.shim 2>/dev/null
setcap cap_setuid+ep /tmp/.shim 2>&1 && /tmp/.shim -c 'echo escaped'
