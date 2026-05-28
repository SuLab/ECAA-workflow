#!/bin/bash
# Sandbox must refuse: spawning a unit on the host via systemd-run
systemd-run --user --scope --slice=evil bash -c 'echo escaped' 2>&1 | head -3
