import { createHash } from 'node:crypto';
import { readFile, stat } from 'node:fs/promises';
import { isAbsolute, relative, resolve } from 'node:path';

export function requireValue(condition, message) {
  if (!condition) throw new Error(message);
}

export function option(name, { required = true, multiple = false } = {}) {
  const prefix = `--${name}=`;
  const values = process.argv.slice(2).filter((argument) => argument.startsWith(prefix)).map((argument) => argument.slice(prefix.length));
  if (required) requireValue(values.length > 0 && values.every(Boolean), `--${name} is required`);
  requireValue(multiple || values.length <= 1, `--${name} may only be provided once`);
  return multiple ? values : values[0];
}

export function safeArtifactPath(root, path) {
  requireValue(typeof path === 'string' && path.length > 0, 'evidence path is missing');
  requireValue(!isAbsolute(path) && !path.includes('\\') && !path.includes(':') && !path.split('/').includes('..'), `unsafe evidence path: ${path}`);
  const absolute = resolve(root, path);
  const relation = relative(root, absolute);
  requireValue(relation !== '..' && !relation.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`), `evidence path escapes root: ${path}`);
  return absolute;
}

export async function sha256File(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

export async function artifact(root, path) {
  const absolute = safeArtifactPath(root, path);
  const info = await stat(absolute);
  requireValue(info.isFile() && info.size > 0, `evidence artifact is missing or empty: ${path}`);
  return { path, size: info.size, sha256: await sha256File(absolute) };
}

export async function readJsonArtifact(root, path) {
  return JSON.parse(await readFile(safeArtifactPath(root, path), 'utf8'));
}

function acceptedModel(report, context) {
  return report?.schema_version === 2
    && report.status === 'completed'
    && report.commit === context.commit
    && report.runner?.ci_run_id === context.run_id
    && report.acceptance?.status === 'accepted';
}

function acceptedFidelity(signoff, screenshot, model) {
  return signoff?.schema_version === 1
    && signoff.status === 'accepted'
    && typeof model?.release_inputs_sha256 === 'string'
    && signoff.release_inputs_sha256 === model.release_inputs_sha256
    && typeof signoff.screenshot_path === 'string'
    && signoff.screenshot_path.length > 0
    && signoff.screenshot_sha256 === screenshot.sha256
    && typeof signoff.reviewer === 'string'
    && signoff.reviewer.trim().length > 0
    && typeof signoff.reviewed_at === 'string'
    && Number.isFinite(Date.parse(signoff.reviewed_at))
    && typeof signoff.notes === 'string'
    && signoff.notes.trim().length > 0;
}

function acceptedInstalledCore(report, installer, context) {
  const checks = report?.checks;
  const requiredChecks = [
    'app_launched', 'clean_shutdown', 'runtime_inventory_verified',
    'installed_worker_core_path', 'uninstall_succeeded', 'user_data_retained',
  ];
  return report?.schema_version === 1
    && report.status === 'accepted'
    && report.commit === context.commit
    && report.workflow === context.workflow
    && report.run_id === context.run_id
    && report.run_attempt === context.run_attempt
    && report.installer_sha256 === installer.sha256
    && checks
    && JSON.stringify(Object.keys(checks).sort()) === JSON.stringify(requiredChecks.sort())
    && Object.values(checks).every((value) => value === true);
}

export async function deriveSignoffs(root, paths, artifacts, context) {
  const [model, fidelity, installed] = await Promise.all([
    readJsonArtifact(root, paths.model_evaluation),
    readJsonArtifact(root, paths.fidelity_signoff),
    readJsonArtifact(root, paths.installed_core_smoke),
  ]);
  return {
    model_evaluation: acceptedModel(model, context) ? 'accepted' : 'rejected',
    rendered_fidelity: acceptedFidelity(fidelity, artifacts.implementation_screenshot, model) ? 'accepted' : (fidelity?.status === 'pending' ? 'pending' : 'rejected'),
    installed_core_path: acceptedInstalledCore(installed, artifacts.installer, context) ? 'accepted' : 'rejected',
  };
}

export function exactKeys(value, keys, label) {
  requireValue(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
  requireValue(JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...keys].sort()), `${label} has an unexpected field or omits a required field`);
}
