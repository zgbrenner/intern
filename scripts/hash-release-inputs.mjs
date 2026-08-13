import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

/**
 * Digest of the committed source, excluding `docs/qa/`, used to bind a
 * rendered-fidelity sign-off to the code it was reviewed against.
 *
 * Derived from the commit's tree rather than the working tree, and that
 * distinction is the whole point. The first version hashed the bytes of every
 * tracked file on disk, which made the gate unsatisfiable: the release build
 * enriches `src-tauri/resources/runtime-assets.json` in place with the vcpkg
 * package ownership and digests it resolves while building Tesseract, taking it
 * from 2,098 bytes as committed to 25,615 bytes on the runner. The digest was
 * computed after that step, so a runner could never agree with any clean
 * checkout, and every sign-off was rejected with "rendered fidelity is not
 * accepted". Identical commit, two digests:
 *
 *   runner   79137300dc4799dadfa57fc418898d4625e0c68a9c90aa5df3e7e09bd29bfc9e
 *   checkout 026d9418637376f8174ab61a04fc0b40a0915e533e9e7cf73ece75fc50fe26c6
 *
 * `git ls-tree` reads the commit directly, so build steps that modify tracked
 * files cannot move it, and neither can a checkout's line-ending settings. The
 * blob object ids it lists are content addressed, so this still changes the
 * moment any committed byte changes - which is what the sign-off must detect.
 */
export function releaseInputsDigest(root = '.') {
  const repositoryRoot = resolve(root);
  const entries = execFileSync('git', ['ls-tree', '-r', '-z', '--full-tree', 'HEAD'], { cwd: repositoryRoot })
    .toString('utf8')
    .split('\0')
    .filter(Boolean)
    // `<mode> <type> <object>\t<path>`
    .filter((entry) => !entry.slice(entry.indexOf('\t') + 1).startsWith('docs/qa/'))
    .sort();
  const hash = createHash('sha256');
  for (const entry of entries) {
    hash.update(entry);
    hash.update('\0');
  }
  return hash.digest('hex');
}

export function currentCommit(root = '.') {
  return execFileSync('git', ['rev-parse', 'HEAD'], { cwd: resolve(root), encoding: 'utf8' }).trim();
}

export function requireAncestor(commit, root = '.') {
  execFileSync('git', ['merge-base', '--is-ancestor', commit, 'HEAD'], { cwd: resolve(root) });
}

const invokedPath = process.argv[1] && resolve(process.argv[1]);
if (invokedPath === fileURLToPath(import.meta.url)) {
  const rootArgument = process.argv.find((argument) => argument.startsWith('--root='));
  process.stdout.write(`${releaseInputsDigest(rootArgument?.slice('--root='.length) ?? '.')}\n`);
}
