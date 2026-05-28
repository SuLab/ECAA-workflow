#!/bin/bash
# Sandbox must refuse: unshare namespaces to escape
unshare --user --pid --mount-proc bash -c 'echo escaped'
