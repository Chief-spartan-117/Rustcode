# Runs a Jupyter kernel for RustCode, through `jupyter_client`, so any kernel that is installed
# (Python with ipykernel, R, Julia, ...) can run the cells of a notebook.
#
# Reads one JSON request per line on stdin:
#   {"op": "execute", "run": 1, "code": "..."}   {"op": "interrupt"}   {"op": "variables"}
# and writes events, one per line, on the real stdout, in the form `notebook_driver.py` uses, plus
#   {"run", "type": "rich", "output": {...}}      an output as a notebook file stores it
#   {"type": "variables", "items": [...]}         {"type": "died", "text": "..."}
# Arguments: the kernel's name and the folder to run in.
import ast
import json
import queue
import sys
import threading

from jupyter_client.manager import KernelManager

protocol = sys.stdout
sys.stdout = sys.stderr
write_lock = threading.Lock()


def send(event):
    with write_lock:
        protocol.write(json.dumps(event) + "\n")
        protocol.flush()


name, folder = sys.argv[1], sys.argv[2]
manager = KernelManager(kernel_name=name)
manager.start_kernel(cwd=folder)
client = manager.client()
client.start_channels()
try:
    client.wait_for_ready(timeout=60)
except RuntimeError:
    send({"type": "died", "text": "the kernel did not start"})
    manager.shutdown_kernel(now=True)
    sys.exit(1)

# What the variables view reads: the user's names, shortened. Only Python kernels understand it.
VARIABLES = (
    "__import__('json').dumps([{'name': n, 'type': type(v).__name__, 'value': __import__('reprlib').repr(v)[:120]}"
    " for n, v in sorted(get_ipython().user_ns.items())"
    " if not n.startswith('_') and n not in get_ipython().user_ns_hidden"
    " and not isinstance(v, __import__('types').ModuleType)])"
)

runs = {}  # message id of a request -> run number
variable_requests = set()
requests = queue.Queue()


def read_requests():
    for raw in sys.stdin:
        raw = raw.strip()
        if raw:
            try:
                requests.put(json.loads(raw))
            except ValueError:
                pass
    requests.put(None)


threading.Thread(target=read_requests, daemon=True).start()


def handle(request):
    op = request.get("op", "execute")
    if op == "execute":
        message = client.execute(request["code"], store_history=True, allow_stdin=False)
        runs[message] = request["run"]
    elif op == "interrupt":
        manager.interrupt_kernel()
    elif op == "variables":
        message = client.execute("", silent=True, user_expressions={"variables": VARIABLES})
        variable_requests.add(message)


def iopub(message):
    parent = message["parent_header"].get("msg_id")
    run = runs.get(parent)
    kind, content = message["msg_type"], message["content"]
    if run is None:
        return
    if kind == "stream":
        send({"run": run, "type": "stream", "name": content["name"], "text": content["text"]})
    elif kind in ("execute_result", "display_data"):
        output = {"output_type": kind, "data": content["data"], "metadata": content.get("metadata", {})}
        if kind == "execute_result":
            output["execution_count"] = content.get("execution_count")
        send({"run": run, "type": "rich", "output": output})
    elif kind == "error":
        send({"run": run, "type": "error", "name": content["ename"], "value": content["evalue"], "traceback": content["traceback"]})
    elif kind == "status" and content["execution_state"] == "idle":
        send({"run": run, "type": "done"})
        del runs[parent]


def shell(message):
    parent = message["parent_header"].get("msg_id")
    if parent not in variable_requests:
        return
    variable_requests.discard(parent)
    try:
        value = message["content"]["user_expressions"]["variables"]["data"]["text/plain"]
        send({"type": "variables", "items": json.loads(ast.literal_eval(value))})
    except Exception:
        pass


alive = True
while alive:
    try:
        while True:
            request = requests.get_nowait()
            if request is None:
                alive = False
                break
            handle(request)
    except queue.Empty:
        pass
    for getter, deliver in ((client.get_iopub_msg, iopub), (client.get_shell_msg, shell)):
        try:
            deliver(getter(timeout=0.02))
        except queue.Empty:
            pass
    if not manager.is_alive():
        send({"type": "died", "text": "the kernel process ended"})
        break

client.stop_channels()
manager.shutdown_kernel(now=True)
