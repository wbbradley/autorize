# pi-digits

End-to-end demo of `autorize`. The harness asks a mock "agent" to nudge the
single number in `value.txt` closer to π over a handful of iterations.

```sh
cp -r examples/pi-digits/. /tmp/pi-demo
cd /tmp/pi-demo
git init -b main
git add .
git -c user.email=a@b -c user.name=a commit -m init
autorize run pi
```

`mock-agent.sh` is a stand-in for a real LLM-driven agent: it deterministically
steps `value.txt` toward π and deliberately regresses every 4th iteration so
you can see both `merged` and `discarded` outcomes in the run log.
