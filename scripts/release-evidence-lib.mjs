import { createHash } from 'node:crypto';
import { readFile, readdir, stat } from 'node:fs/promises';
import { basename, dirname, isAbsolute, relative, resolve } from 'node:path';

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
    && report.pipeline === 'new'
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

export async function validateSpdx(root, entry, { kind, releaseVersion }) {
  const sbom = await readJsonArtifact(root, entry.path);
  requireValue(typeof sbom.spdxVersion === 'string' && sbom.spdxVersion.startsWith('SPDX-'), `${entry.path} is not an SPDX document`);
  requireValue(typeof sbom.SPDXID === 'string' && sbom.SPDXID.startsWith('SPDXRef-'), `${entry.path} has no SPDX document identity`);
  requireValue(typeof sbom.name === 'string' && sbom.name.trim().length > 0, `${entry.path} has no SPDX document name`);
  requireValue(typeof sbom.documentNamespace === 'string' && sbom.documentNamespace.length > 0, `${entry.path} has no SPDX document namespace`);
  requireValue((Array.isArray(sbom.packages) && sbom.packages.length > 0) || (Array.isArray(sbom.files) && sbom.files.length > 0), `${entry.path} has no SPDX package or file`);
  if (kind === 'application') {
    requireValue(sbom.name.includes(releaseVersion), `${entry.path} does not identify the release version`);
  } else {
    requireValue(Array.isArray(sbom.packages) && sbom.packages.some((pkg) => (
      typeof pkg?.name === 'string' && pkg.name.trim().length > 0
      && typeof pkg?.versionInfo === 'string' && pkg.versionInfo.trim().length > 0
    )), `${entry.path} has no runtime component name and pinned version identity`);
  }
}

export async function validateSha256Sums(root, checksums, excluded = ['SHA256SUMS.txt', 'release-evidence-manifest.json']) {
  const absolute = safeArtifactPath(root, checksums.path);
  requireValue(basename(absolute) === 'SHA256SUMS.txt', 'checksum artifact must be named SHA256SUMS.txt');
  const releaseDirectory = dirname(absolute);
  const lines = (await readFile(absolute, 'utf8')).split(/\r?\n/).filter(Boolean);
  requireValue(lines.length > 0, 'SHA256SUMS.txt is empty');
  const entries = new Map();
  for (const line of lines) {
    const match = /^([a-f0-9]{64})  ([^\\/:]+)$/.exec(line);
    requireValue(match, `malformed SHA256SUMS entry: ${line}`);
    const [, hash, leaf] = match;
    requireValue(!excluded.includes(leaf), `SHA256SUMS must exclude ${leaf} to avoid a digest cycle`);
    requireValue(!entries.has(leaf), `duplicate SHA256SUMS filename: ${leaf}`);
    entries.set(leaf, hash);
  }
  const published = (await readdir(releaseDirectory, { withFileTypes: true }))
    .filter((entry) => entry.isFile() && !excluded.includes(entry.name))
    .map((entry) => entry.name)
    .sort();
  requireValue(JSON.stringify([...entries.keys()].sort()) === JSON.stringify(published), `SHA256SUMS must cover every distributable release file exactly once (listed: ${[...entries.keys()].sort().join(', ')}; published: ${published.join(', ')})`);
  for (const leaf of published) {
    const current = await sha256File(resolve(releaseDirectory, leaf));
    requireValue(entries.get(leaf) === current, `SHA256SUMS hash does not match ${leaf}`);
  }
}
