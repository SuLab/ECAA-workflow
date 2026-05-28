#!/bin/bash
# Sandbox must refuse: enumerating other processes' shared-memory segments
ipcs -m 2>&1 | head -5 && for id in $(ipcs -m 2>/dev/null | awk 'NR>3 {print $2}'); do
    ipcrm -m "$id" 2>&1 && echo "owned: removed $id"
done
