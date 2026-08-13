import { execFileSync } from 'node:child_process';
import { mkdtemp, writeFile, mkdir, readFile, appendFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { releaseInputsDigest } from './hash-release-inputs.mjs';

/**
 * The digest binds a rendered-fidelity sign-off to the code it was reviewed
 * against, so it has to be a property of the commit and nothing else.
 *
 * The original implementation hashed working-tree bytes, which made the gate
 * unsatisfiable in practice: `scripts/fetch-windows-assets.ps1` rewrites the
 * tracked `src-tauri/resources/runtime-assets.json` in place while building, so
 * the release runner computed a different digest from any clean checkout of the
 * same commit and rejected every sign-off. These tests pin both halves of the
 * property that fixes it.
 */
async function repository(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), 'digest-'));
  const git = (...args: string[]) => execFileSync('git', args, { cwd: root });
  git('init', '-q');
  git('config', 'user.email', 'test@example.com');
  git('config', 'user.name', 'Test');
  await writeFile(join(root, 'source.txt'), 'committed source\n');
  await mkdir(join(root, 'docs', 'qa'), { recursive: true });
  await writeFile(join(root, 'docs', 'qa', 'signoff.json'), '{"status":"accepted"}\n');
  await mkdir(join(root, 'src-tauri', 'resources'), { recursive: true });
  await writeFile(join(root, 'src-tauri', 'resources', 'runtime-assets.json'), '{"bundled_files":[]}\n');
  git('add', '-A');
  git('commit', '-qm', 'initial');
  return root;
}

describe('release inputs digest', () => {
  it('does not move when a build step rewrites a tracked file', async () => {
    const root = await repository();
    const before = releaseInputsDigest(root);

    // Exactly what the asset fetch does to runtime-assets.json on the runner.
    const manifest = join(root, 'src-tauri', 'resources', 'runtime-assets.json');
    await writeFile(manifest, `{"bundled_files":[${'{"install_path":"tesseract.exe"},'.repeat(40)}{}]}\n`);
    expect((await readFile(manifest, 'utf8')).length).toBeGreaterThan(1000);

    expect(releaseInputsDigest(root)).toBe(before);
  });

  it('moves when a tracked file is actually committed differently', async () => {
    const root = await repository();
    const before = releaseInputsDigest(root);

    await appendFile(join(root, 'source.txt'), 'a real change\n');
    execFileSync('git', ['add', '-A'], { cwd: root });
    execFileSync('git', ['commit', '-qm', 'change'], { cwd: root });

    // The gate exists to catch this: reviewed code changed, so the sign-off must
    // stop matching.
    expect(releaseInputsDigest(root)).not.toBe(before);
  });

  it('ignores docs/qa so a sign-off can be recorded without invalidating itself', async () => {
    const root = await repository();
    const before = releaseInputsDigest(root);

    await writeFile(join(root, 'docs', 'qa', 'signoff.json'), '{"status":"accepted","note":"reviewed"}\n');
    execFileSync('git', ['add', '-A'], { cwd: root });
    execFileSync('git', ['commit', '-qm', 'record signoff'], { cwd: root });

    expect(releaseInputsDigest(root)).toBe(before);
  });
});
