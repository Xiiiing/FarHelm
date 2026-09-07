"""Local training example; only Python's standard library and farhelm-agent are needed."""
import os
import subprocess
import sys
import traceback
import uuid

project = os.environ.get("FARHELM_PROJECT", "cc08")
run_id = os.environ.get("FARHELM_RUN_ID", f"batch-{uuid.uuid4().hex}")
training_exit = 0
try:
    for round_number in range(1, 9):
        subprocess.run([sys.executable, "train.py", "--round", str(round_number)], check=True)
except subprocess.CalledProcessError as error:
    training_exit = error.returncode if error.returncode >= 0 else 128 - error.returncode
except KeyboardInterrupt:
    training_exit = 130
except Exception:
    training_exit = 1
    traceback.print_exc()
finally:
    try:
        subprocess.run(
            ["farhelm-agent", "experiment", "report", "--project", project,
             "--run-id", run_id, "--name", "8 rounds of training",
             "--exit-code", str(training_exit), "--message", "Training batch ended"],
            check=True,
        )
    except (OSError, subprocess.CalledProcessError):
        print(f"FarHelm could not save the report; retry run {run_id} with exit code {training_exit}.", file=sys.stderr)

sys.exit(training_exit)
