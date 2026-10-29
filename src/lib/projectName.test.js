import { describe, expect, it } from 'vitest';
import { dirOf, joinPath, normalizeProjectName, projectFileName, randomProjectName } from './projectName.js';

describe('normalizeProjectName', () => {
  it('turns spaces into underscores', () => {
    expect(normalizeProjectName('project pentest x')).toBe('project_pentest_x');
    expect(normalizeProjectName('  a   b\tc  ')).toBe('a_b_c');
  });

  it('drops characters a file name cannot carry', () => {
    expect(normalizeProjectName('Pentest: Bank XYZ (2026)')).toBe('Pentest_Bank_XYZ_2026');
    expect(normalizeProjectName('a/b\\c*?"<>|d')).toBe('a_b_c_d');
    expect(normalizeProjectName('../../etc/passwd')).toBe('etc_passwd');
  });

  it('keeps case, digits, dashes, dots and non-latin letters', () => {
    expect(normalizeProjectName('Web-App v1.2')).toBe('Web-App_v1.2');
    expect(normalizeProjectName('uji coba ñandú')).toBe('uji_coba_ñandú');
  });

  it('strips a typed extension and dangling separators', () => {
    expect(normalizeProjectName('demo.ngintip')).toBe('demo');
    expect(normalizeProjectName('.hidden_')).toBe('hidden');
    expect(normalizeProjectName('___')).toBe('');
    expect(normalizeProjectName('')).toBe('');
  });

  it('avoids reserved Windows names and caps the length', () => {
    expect(normalizeProjectName('con')).toBe('con_');
    expect(normalizeProjectName('x'.repeat(200))).toHaveLength(80);
  });
});

describe('randomProjectName', () => {
  it('is day-month-year plus a random tail', () => {
    const name = randomProjectName(new Date(2026, 9, 3));
    expect(name).toMatch(/^03-10-2026-[a-z0-9]{6}$/);
    expect(normalizeProjectName(name)).toBe(name);
  });

  it('differs between calls', () => {
    const seen = new Set(Array.from({ length: 20 }, () => randomProjectName()));
    expect(seen.size).toBeGreaterThan(15);
  });
});

describe('projectFileName / paths', () => {
  it('adds the extension, or gives nothing for an empty name', () => {
    expect(projectFileName('project pentest x')).toBe('project_pentest_x.ngintip');
    expect(projectFileName('  ')).toBe('');
  });

  it('joins and splits with the folder separator', () => {
    expect(joinPath('/Users/me/Documents/Ngintip', 'a.ngintip')).toBe('/Users/me/Documents/Ngintip/a.ngintip');
    expect(joinPath('/tmp/', 'a.ngintip')).toBe('/tmp/a.ngintip');
    expect(joinPath('C:\\Users\\me', 'a.ngintip')).toBe('C:\\Users\\me\\a.ngintip');
    expect(dirOf('/tmp/x/a.ngintip')).toBe('/tmp/x');
    expect(dirOf('C:\\me\\a.ngintip')).toBe('C:\\me');
    expect(dirOf('a.ngintip')).toBe('');
  });
});
