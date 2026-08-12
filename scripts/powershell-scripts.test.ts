import { execFile } from 'node:child_process';
import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { expect, it } from 'vitest';

const exec = promisify(execFile);

/**
 * Every packaging and evaluation script is PowerShell that only runs on a
 * Windows runner, late in a job that first spends twenty minutes building
 * Tesseract. Two of them shipped with parse errors that no local check could
 * catch, so neither had ever executed: one used `$Code:` and one `$OutputPath:`,
 * which PowerShell reads as scope-qualified variables like `$env:PATH`.
 *
 * Parsing is cheap and catches that class outright. On a machine without
 * PowerShell this skips rather than pretends to pass.
 */
async function powershell(): Promise<string | undefined> {
  for (const candidate of ['pwsh', 'powershell']) {
    try {
      await exec(candidate, ['-NoProfile', '-Command', '$PSVersionTable.PSVersion.Major']);
      return candidate;
    } catch {
      // try the next one
    }
  }
  return undefined;
}

it('every PowerShell script parses', async () => {
  const shell = await powershell();
  if (!shell) {
    expect(process.platform).not.toBe('win32');
    return;
  }

  const scripts = (await readdir('scripts')).filter((name) => name.endsWith('.ps1')).sort();
  expect(scripts.length).toBeGreaterThan(4);

  // The paths are embedded rather than passed as arguments: `-Command` does not
  // populate $args, and a checker that silently received no paths would report
  // success without having parsed anything.
  const list = scripts.map((name) => `'${join('scripts', name).replaceAll('\\', '\\\\')}'`).join(',');
  const script =
    `$paths = @(${list});` +
    `if ($paths.Count -ne ${scripts.length}) { Write-Output 'path list did not survive'; exit 1 };` +
    `foreach ($path in $paths) {` +
    `  $errors = $null;` +
    `  [void][System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path $path), [ref]$null, [ref]$errors);` +
    `  if ($errors.Count -gt 0) { Write-Output ($path + ' line ' + $errors[0].Extent.StartLineNumber + ': ' + $errors[0].Message) }` +
    `}`;

  const { stdout } = await exec(shell, ['-NoProfile', '-Command', script]);
  expect(stdout.trim()).toBe('');
}, 60_000);
