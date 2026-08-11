import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

export function releaseInputsDigest(root = '.') {
  const repositoryRoot = resolve(root);
  const tracked = execFileSync('git', ['ls-files', '-z'], { cwd: repositoryRoot })
    .toString('utf8')
    .split('\0')
    .filter((path) => path && !path.startsWith('docs/qa/'))
    .sort();
  const hash = createHash('sha256');
  for (const path of tracked) {
    hash.update(path);
    hash.update('\0');
    hash.update(readFileSync(resolve(repositoryRoot, path)));
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
