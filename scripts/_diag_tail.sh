#!/usr/bin/env bash
# Fast non-containerized diagnostic for the breadth tail failures.
# Emits, registers data, runs harness with ECAA_DISABLE_CONTAINERS=1 (no docker
# overhead), then reports per-task final state + the agent error for any task
# that did not complete. Output -> /tmp/diag_tail_<name>.txt
set -uo pipefail
cd /home/a/scripps/ecaa-workflow
source scripts/_campaign-env.sh
export ECAA_DISABLE_CONTAINERS=1
CLI=./target/release/ecaa-workflow
HARNESS=./target/release/ecaa-workflow-harness
name="$1"
data="testdata/scenarios/atoms/$name"
pkg="/tmp/diag/$name"
out="/tmp/diag_tail_$name.txt"
prose="$2"
rm -rf "$pkg"; mkdir -p "$pkg"
printf '%s\n' "$prose" > "/tmp/diag_$name.intake.txt"
"$CLI" intake --input "/tmp/diag_$name.intake.txt" --output "$pkg" --config config > "/tmp/diag_$name.emit.log" 2>&1
if [ -d "$data" ]; then
  ECAA_RS_PKG="$pkg" ECAA_RS_DATA="$data" ECAA_RS_NAME="$name" python3 - <<'PY'
import json, os
pkg=os.environ["ECAA_RS_PKG"]; data=os.environ["ECAA_RS_DATA"]; name=os.environ["ECAA_RS_NAME"]
files=[{"relpath":f,"size_bytes":os.path.getsize(os.path.join(data,f))}
       for f in sorted(os.listdir(data)) if os.path.isfile(os.path.join(data,f)) and f!="SHA256SUMS"]
inp=[{"input_id":"in_fixture","label":name,"kind":"local_path","root_path":os.path.abspath(data),"files":files}]
os.makedirs(os.path.join(pkg,"runtime"),exist_ok=True)
json.dump(inp,open(os.path.join(pkg,"runtime","inputs.json"),"w"),indent=2)
PY
fi
"$HARNESS" --package "$pkg" --agent scripts/agent-fixture-plots.sh --max-iterations 120 > "$pkg/harness.log" 2>&1
ECAA_RS_PKG="$pkg" ECAA_RS_OUT="$out" python3 - <<'PY'
import json, os
pkg=os.environ["ECAA_RS_PKG"]; out=os.environ["ECAA_RS_OUT"]
wf=json.load(open(os.path.join(pkg,"WORKFLOW.json"))); T=wf["tasks"]
def st(t):
    s=t.get("state",{})
    return s.get("status") if isinstance(s,dict) else s
lines=[]
done=sum(1 for t in T.values() if st(t)=="completed")
lines.append(f"COMPLETED {done}/{len(T)}")
for k,t in sorted(T.items()):
    s=st(t)
    if s=="completed": continue
    lines.append(f"  {k} = {s}")
    # progress log + blocker
    pl=os.path.join(pkg,"runtime","outputs",k,"progress.log")
    bl=os.path.join(pkg,"runtime","outputs",k,"blocker.json")
    if os.path.exists(bl):
        try: lines.append(f"      blocker: {json.load(open(bl)).get('reason','?')[:200]}")
        except: pass
    if os.path.exists(pl):
        tail=open(pl).read().strip().splitlines()[-2:]
        for L in tail: lines.append(f"      log: {L[:200]}")
open(out,"w").write("\n".join(lines)+"\n")
print(out)
PY
echo "DIAG_DONE_$name"
