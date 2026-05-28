#!/bin/bash
# Sandbox must refuse: snooping the system D-Bus (cross-service IPC eavesdrop)
dbus-monitor --system 2>&1 | head -3 && echo "exfil: dbus snoop opened"
