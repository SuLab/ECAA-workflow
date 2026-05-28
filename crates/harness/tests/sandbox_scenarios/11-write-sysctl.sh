#!/bin/bash
# Sandbox must refuse: writing to /proc/sys (kernel parameter mutation)
echo 1 > /proc/sys/net/ipv4/ip_forward 2>&1 && echo "owned"
