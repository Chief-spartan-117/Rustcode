# Runs notebook cells for RustCode. Reads one JSON request per line on stdin:
#   {"run": 1, "code": "..."}
# and writes JSON events, one per line, on the real stdout. The variables stay between runs.
import ast
import base64
import io
import json
import os
import reprlib
import subprocess
import types
import sys
import traceback

os.environ.setdefault("MPLBACKEND", "Agg")
protocol = sys.stdout
user_globals = {"__name__": "__main__", "__builtins__": __builtins__}
counter = 0


def send(event):
    protocol.write(json.dumps(event) + "\n")
    protocol.flush()


class Stream(io.TextIOBase):
    def __init__(self, run, name):
        self.run = run
        self.name = name

    def writable(self):
        return True

    def write(self, text):
        if text:
            send({"run": self.run, "type": "stream", "name": self.name, "text": text})
        return len(text)

    def flush(self):
        pass


def magic(line, run):
    """`!cmd` runs a shell command; `%pip ...` runs pip with this interpreter. Else None."""
    stripped = line.strip()
    if stripped.startswith("!"):
        command = stripped[1:]
    elif stripped.startswith("%pip "):
        command = '"%s" -m pip %s' % (sys.executable, stripped[5:])
    elif stripped.startswith("%"):
        return ""  # %matplotlib inline and the like: nothing to do
    else:
        return None
    done = subprocess.run(command, shell=True, capture_output=True, text=True)
    if done.stdout:
        send({"run": run, "type": "stream", "name": "stdout", "text": done.stdout})
    if done.stderr:
        send({"run": run, "type": "stream", "name": "stderr", "text": done.stderr})
    return ""


def preprocess(code, run):
    lines = []
    for line in code.split("\n"):
        replaced = magic(line, run) if line.lstrip()[:1] in ("!", "%") else None
        lines.append(line if replaced is None else "")
    return "\n".join(lines)


def show_figures(run):
    pyplot = sys.modules.get("matplotlib.pyplot")
    if pyplot is None:
        return
    for number in list(pyplot.get_fignums()):
        buffer = io.BytesIO()
        pyplot.figure(number).savefig(buffer, format="png", bbox_inches="tight")
        data = base64.b64encode(buffer.getvalue()).decode("ascii")
        send({"run": run, "type": "image", "mime": "image/png", "data": data})
    pyplot.close("all")


def variables():
    items = []
    short = reprlib.Repr()
    short.maxstring = short.maxother = 100
    for name in sorted(user_globals):
        value = user_globals[name]
        if name.startswith("_") or isinstance(value, types.ModuleType):
            continue
        try:
            text = short.repr(value)
        except BaseException:
            text = "<cannot show>"
        items.append({"name": name, "type": type(value).__name__, "value": text.replace("\n", " ")})
    send({"type": "variables", "items": items})


def execute(run, code):
    global counter
    out, err = Stream(run, "stdout"), Stream(run, "stderr")
    old = sys.stdout, sys.stderr, sys.stdin
    sys.stdout, sys.stderr, sys.stdin = out, err, io.StringIO("")
    try:
        code = preprocess(code, run)
        tree = ast.parse(code, "<cell>", "exec")
        last = None
        if tree.body and isinstance(tree.body[-1], ast.Expr):
            last = ast.Expression(tree.body.pop().value)
        exec(compile(tree, "<cell>", "exec"), user_globals)
        if last is not None:
            value = eval(compile(last, "<cell>", "eval"), user_globals)
            if value is not None:
                user_globals["_"] = value
                send({"run": run, "type": "result", "text": repr(value)})
    except BaseException as error:  # includes KeyboardInterrupt from an interrupt
        if isinstance(error, SyntaxError):
            lines = traceback.format_exception_only(type(error), error)
        else:
            frames = error.__traceback__.tb_next if error.__traceback__ else None
            lines = traceback.format_exception(type(error), error, frames)
        send({
            "run": run, "type": "error", "name": type(error).__name__, "value": str(error),
            "traceback": "".join(lines).rstrip("\n").split("\n"),
        })
    finally:
        sys.stdout, sys.stderr, sys.stdin = old
        try:
            show_figures(run)
        except BaseException:
            pass
        send({"run": run, "type": "done"})


for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    try:
        request = json.loads(raw)
    except ValueError:
        continue
    try:
        if request.get("op") == "variables":
            variables()
            continue
        execute(request["run"], request["code"])
    except KeyboardInterrupt:
        send({"run": request.get("run", 0), "type": "done"})
