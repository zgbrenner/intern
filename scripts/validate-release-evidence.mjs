import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { artifact, deriveSignoffs, exactKeys, option, requireValue, safeArtifactPath } from './release-evidence-lib.mjs';

const positional = process.argv.slice(2).find((argument) => !argument.startsWith('--'));
requireValue(positional, 'release evidence manifest path is required');
const root = resolve(option('root'));
const allowPending = process.argv.includes('--allow-pending');
const expected = {
  commit: option('commit'),
  workflow: option('workflow'),
  run_id: option('run-id'),
  run_attempt: option('run-attempt'),
};
const manifest = JSON.parse(await readFile(resolve(positional), 'utf8'));

exactKeys(manifest, ['schema_version', 'status', 'subject', 'artifacts', 'signoffs'], 'release evidence manifest');
requireValue(manifest.schema_version === 1, 'release evidence schema_version must be 1');
requireValue(['accepted', 'blocked'].includes(manifest.status), 'release evidence status is invalid');
exactKeys(manifest.subject, ['commit', 'workflow', 'run_id', 'run_attempt'], 'release evidence subject');
for (const [field, value] of Object.entries(expected)) requireValue(manifest.subject[field] === value, `release evidence ${field} does not match this run`);
exactKeys(manifest.artifacts, ['model_evaluation', 'implementation_screenshot', 'release_checklist', 'fidelity_signoff', 'installed_core_smoke', 'installer', 'logs'], 'release evidence artifacts');
exactKeys(manifest.signoffs, ['model_evaluation', 'rendered_fidelity', 'installed_core_path'], 'release evidence signoffs');
requireValue(Array.isArray(manifest.artifacts.logs) && manifest.artifacts.logs.length > 0, 'release evidence logs are missing');

async function verifyEntry(entry, label) {
  exactKeys(entry, ['path', 'size', 'sha256'], label);
  requireValue(Number.isSafeInteger(entry.size) && entry.size > 0, `${label} size is invalid`);
  requireValue(/^[a-f0-9]{64}$/.test(entry.sha256), `${label} hash is invalid`);
  const current = await artifact(root, entry.path);
  requireValue(current.size === entry.size && current.sha256 === entry.sha256, `${label} hash or size does not match its artifact`);
}
for (const key of ['model_evaluation', 'implementation_screenshot', 'release_checklist', 'fidelity_signoff', 'installed_core_smoke', 'installer']) {
  await verifyEntry(manifest.artifacts[key], key);
}
for (const [index, log] of manifest.artifacts.logs.entries()) await verifyEntry(log, `log ${index}`);

const paths = {
  model_evaluation: manifest.artifacts.model_evaluation.path,
  fidelity_signoff: manifest.artifacts.fidelity_signoff.path,
  installed_core_smoke: manifest.artifacts.installed_core_smoke.path,
};
const derived = await deriveSignoffs(root, paths, manifest.artifacts, manifest.subject);
requireValue(JSON.stringify(manifest.signoffs) === JSON.stringify(derived), 'release evidence sign-offs are not derived from their artifacts');
if (derived.model_evaluation !== 'accepted') throw new Error('production model evaluation is not accepted');
if (derived.rendered_fidelity !== 'accepted' && !(allowPending && derived.rendered_fidelity === 'pending')) throw new Error('rendered fidelity is not accepted; release is blocked');
if (derived.installed_core_path !== 'accepted') throw new Error('installed core path is not accepted; release is blocked');
const accepted = Object.values(derived).every((status) => status === 'accepted');
requireValue(manifest.status === (accepted ? 'accepted' : 'blocked'), 'release evidence status does not match sign-offs');
if (!accepted && !allowPending) throw new Error('release evidence is blocked');
safeArtifactPath(root, manifest.artifacts.installer.path);
process.stdout.write(`${JSON.stringify({ status: accepted ? 'accepted' : 'blocked', commit: manifest.subject.commit, run_id: manifest.subject.run_id })}\n`);
