// Research driver with fixed read-only fixture tools, not Rook dispatch.
import { dependency, execute } from './runtime.mjs';

const chunks = [];
let bytes = 0;
for await (const chunk of process.stdin) {
  bytes += chunk.length;
  if (bytes > 1024 * 1024) throw new Error('fixture input limit reached');
  chunks.push(chunk);
}
const request = JSON.parse(Buffer.concat(chunks).toString('utf8'));
const names = Object.keys(request.files);
if (names.length > 128 || names.some(name => name.length > 256)) throw new Error('fixture file limit');
const engine = await dependency(process.env.ROOK_QUICKJS_DIRECTORY);
const result = await execute(engine, request.code, async (name, args) => {
  if (name === 'list_dir') return JSON.stringify({ content: JSON.stringify(names.sort()), is_error: false });
  if (name === 'read_file' && Object.hasOwn(request.files, args.path)) {
    return JSON.stringify({ content: request.files[args.path], is_error: false });
  }
  return JSON.stringify({ content: 'no such fixture file', is_error: true });
});
process.stdout.write(JSON.stringify(result));
