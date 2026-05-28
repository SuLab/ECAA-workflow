#!/bin/bash
# Sandbox must refuse: sending SIGKILL to PID 1 (would tear down init outside the sandbox)
kill -9 1 2>&1 && echo "escaped: SIGKILL delivered to PID 1"
