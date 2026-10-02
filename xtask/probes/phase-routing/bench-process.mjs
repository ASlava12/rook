import fs from 'node:fs';
import { spawn } from 'node:child_process';

// The files remain readable while inference is waiting. Admit each chunk before
// copying it into the fixed buffers or writing retained evidence.
export function turn(executable, args, env, files, timeout = 900000) {
  return new Promise(resolve => {
    const captures = {
      stdout: { buffer: Buffer.alloc(1024 * 1024), bytes: 0, fd: fs.openSync(files.stdout, 'wx') },
      stderr: { buffer: Buffer.alloc(3 * 1024 * 1024), bytes: 0, fd: fs.openSync(files.stderr, 'wx') },
    };
    const child = spawn(executable, args, { env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    if (child.pid) fs.writeFileSync(files.pid, String(child.pid));
    let error, escalation;
    function stop(code) {
      if (error) return;
      error = { code }; child.kill();
      escalation = setTimeout(() => {
        if (child.exitCode == null && child.signalCode == null) child.kill('SIGKILL');
      }, 5000);
    }
    const deadline = setTimeout(() => stop('ETIMEDOUT'), timeout);
    for (const [name, capture] of Object.entries(captures)) child[name].on('data', chunk => {
      if (error) return;
      if (chunk.length > capture.buffer.length - capture.bytes) { stop('ENOBUFFER'); return; }
      chunk.copy(capture.buffer, capture.bytes); capture.bytes += chunk.length;
      try { if (fs.writeSync(capture.fd, chunk) !== chunk.length) stop('EWRITE'); } catch { stop('EWRITE'); }
    });
    child.on('error', why => { error ??= { code: why.code }; });
    child.on('close', (status, signal) => {
      clearTimeout(deadline); clearTimeout(escalation);
      for (const capture of Object.values(captures)) fs.closeSync(capture.fd);
      resolve({ status, signal, error,
        stdout: captures.stdout.buffer.toString('utf8', 0, captures.stdout.bytes),
        stderr: captures.stderr.buffer.toString('utf8', 0, captures.stderr.bytes) });
    });
  });
}
