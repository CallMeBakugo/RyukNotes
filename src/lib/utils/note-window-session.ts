import { dbAttachVault, looksLikeDbVault, setDbMode, DbWriteError } from '../api-db';
import { readNote } from '../api';
import type { NoteContent } from '../types';

/** Bootstrap this webview before any vault read or Editor mount. */
export async function openNoteWindowSession(vaultPath: string, path: string): Promise<NoteContent> {
  setDbMode(false);
  const identity = looksLikeDbVault(vaultPath) ? await dbAttachVault(vaultPath) : null;
  const content = await readNote(vaultPath, path);
  if (identity) {
    const accepted = content.meta.accepted;
    if (!accepted || accepted.vault.path !== identity.path || accepted.vault.file !== identity.file) {
      throw new DbWriteError('vault_replaced', 'The originating vault changed while opening this note window.');
    }
  }
  return content;
}
