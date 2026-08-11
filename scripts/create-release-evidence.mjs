import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { artifact, deriveSignoffs, option, requireValue } from './release-evidence-lib.mjs';

const root = resolve(option('root'));
const output = resolve(option('output'));
const context = {
  commit: option('commit'),
  workflow: option('workflow'),
  run_id: option('run-id'),
  run_attempt: option('run-attempt'),
};
requireValue(/^[a-f0-9]{40}$/.test(context.commit), 'commit must be a full lowercase Git SHA');
for (const [key, value] of Object.entries(context).filter(([key]) => key !== 'commit')) requireValue(value.length > 0, `${key} is required`);

const paths = {
  model_evaluation: option('model-evaluation'),
  implementation_screenshot: option('screenshot'),
  release_checklist: option('checklist'),
  fidelity_signoff: option('fidelity-signoff'),
  installed_core_smoke: option('installed-core-smoke'),
  installer: option('installer'),
};
const logs = option('log', { multiple: true });
requireValue(logs.length > 0, 'at least one --log is required');

const artifacts = {
  model_evaluation: await artifact(root, paths.model_evaluation),
  implementation_screenshot: await artifact(root, paths.implementation_screenshot),
  release_checklist: await artifact(root, paths.release_checklist),
  fidelity_signoff: await artifact(root, paths.fidelity_signoff),
  installed_core_smoke: await artifact(root, paths.installed_core_smoke),
  installer: await artifact(root, paths.installer),
  logs: await Promise.all([...new Set(logs)].sort().map((path) => artifact(root, path))),
};
const signoffs = await deriveSignoffs(root, paths, artifacts, context);
const manifest = {
  schema_version: 1,
  status: Object.values(signoffs).every((status) => status === 'accepted') ? 'accepted' : 'blocked',
  subject: context,
  artifacts,
  signoffs,
};
await mkdir(dirname(output), { recursive: true });
await writeFile(output, `${JSON.stringify(manifest, null, 2)}\n`);
process.stdout.write(`${JSON.stringify({ status: manifest.status, output })}\n`);
