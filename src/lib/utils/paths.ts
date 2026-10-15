function pathParts(path: string): string[] {
	return path.replace(/\\/g, '/').split('/').filter(Boolean);
}

function pathRoot(path: string): string {
	const normalized = path.replace(/\\/g, '/');
	const drive = normalized.match(/^([A-Za-z]:)\//);
	if (drive) return drive[1].toLowerCase();
	return normalized.startsWith('/') ? '/' : '';
}

export function relativePath(fromDirectory: string, targetPath: string): string {
	const from = pathParts(fromDirectory);
	const target = pathParts(targetPath);
	if (pathRoot(fromDirectory) !== pathRoot(targetPath)) return targetPath.replace(/\\/g, '/');
	const windowsPath = /^[A-Za-z]:[\\/]/.test(fromDirectory) || /^[A-Za-z]:[\\/]/.test(targetPath);
	const equal = windowsPath
		? (left: string, right: string) => left.toLowerCase() === right.toLowerCase()
		: (left: string, right: string) => left === right;

	let common = 0;
	while (common < from.length && common < target.length && equal(from[common], target[common])) {
		common++;
	}

	const result = [...Array(from.length - common).fill('..'), ...target.slice(common)];
	return result.join('/') || '.';
}

export function normalizeLocalAssetPath(path: string): string {
	return path
		.replace(/\\/g, '/')
		.replace(/^\/([A-Za-z]:\/)/, '$1');
}

export function assetUrlToLocalPath(source: string): string | null {
	if (!source.startsWith('asset:') && !source.startsWith('http://asset.localhost') && !source.startsWith('https://asset.localhost')) {
		return null;
	}
	try {
		const path = normalizeLocalAssetPath(decodeURIComponent(new URL(source).pathname));
		// Tauri prepends a URL-path slash to POSIX and UNC paths.
		return path.startsWith('//') ? path.substring(1) : path;
	} catch {
		return null;
	}
}

/** The owning configured vault, never the active vault of an unrelated external note. */
export function noteAssetRoot(notePath: string | null, activeVault: string | null, vaults: readonly string[] = []): string | null {
	if (!notePath || isPathInDirectory(notePath, activeVault)) return activeVault;
	return [...vaults].filter(root => isPathInDirectory(notePath, root)).sort((a, b) => b.length - a.length)[0] ?? null;
}

/** Decode only the URI path, keeping query/fragment data out of filesystem paths. */
export function resolveFileReference(source: string, notePath: string | null, vaultRoot: string | null): { path: string; suffix: string } {
	const boundary = source.search(/[?#]/);
	const encoded = boundary < 0 ? source : source.slice(0, boundary);
	let path: string;
	try { path = decodeURIComponent(encoded); } catch { path = encoded; }
	return { path: resolveVaultFilePath(path, notePath, vaultRoot), suffix: boundary < 0 ? '' : source.slice(boundary) };
}

/** Percent-encode each path segment for a Markdown destination; a Windows drive prefix stays literal. */
export function encodeMarkdownPath(path: string): string {
	const normalized = path.replace(/\\/g, '/');
	const drive = /^[A-Za-z]:(?=\/|$)/.exec(normalized)?.[0] ?? '';
	return drive + normalized.slice(drive.length).split('/').map(segment => encodeURIComponent(segment)
		.replace(/[!'()*]/g, character => `%${character.charCodeAt(0).toString(16).toUpperCase()}`)).join('/');
}

export function resolveVaultFilePath(
	targetPath: string,
	notePath: string | null,
	vaultRoot: string | null,
): string {
	const target = normalizeLocalAssetPath(targetPath);
	if (target.startsWith('/') || /^[A-Za-z]:\//.test(target)) return target;

	const root = vaultRoot?.replace(/\\/g, '/').replace(/\/$/, '') ?? '';
	if (root) {
		// Older imported managed links may still contain notebook-depth prefixes.
		const managed = target.match(/^(?:\.{1,2}\/)*(\.ryuknotes\/attachments\/.*)$/);
		if (managed) return resolvePathFromFile(`${root}/.vault-root`, managed[1]);
		if (target.startsWith('.ryuknotes/')) return resolvePathFromFile(`${root}/.vault-root`, target);
	}
	if (notePath) return resolvePathFromFile(notePath, target);
	if (root) return resolvePathFromFile(`${root}/.vault-root`, target);
	return target;
}

export function assetSourceToMarkdown(
	source: string,
	notePath: string | null,
	vaultRoot: string | null,
): string {
	if (source.startsWith('blob:')) return '';
	if (source.startsWith('imgproxy:') || source.startsWith('http://imgproxy.localhost') || source.startsWith('https://imgproxy.localhost')) {
		try {
			return decodeURIComponent(new URL(source).pathname.substring(1));
		} catch {
			return source;
		}
	}
	const absolutePath = assetUrlToLocalPath(source);
	if (absolutePath === null) return source;

	const url = new URL(source);
	const suffix = url.search + url.hash;
	const normalizedRoot = vaultRoot?.replace(/\\/g, '/').replace(/\/$/, '');
	if (!normalizedRoot || !isPathInDirectory(absolutePath, normalizedRoot)) return encodeMarkdownPath(absolutePath) + suffix;
	const vaultRelative = absolutePath.substring(normalizedRoot.length + 1);
	if (vaultRelative.startsWith('.ryuknotes/') || !notePath) return encodeMarkdownPath(vaultRelative) + suffix;
	const normalizedNotePath = notePath.replace(/\\/g, '/');
	const noteDirectory = normalizedNotePath.substring(0, normalizedNotePath.lastIndexOf('/'));
	return encodeMarkdownPath(relativePath(noteDirectory, absolutePath)) + suffix;
}

export function resolvePathFromFile(filePath: string, targetPath: string): string {
	const normalizedFile = filePath.replace(/\\/g, '/');
	const normalizedTarget = targetPath.replace(/\\/g, '/');
	const lastSeparator = normalizedFile.lastIndexOf('/');
	const fileDirectory = lastSeparator >= 0 ? normalizedFile.slice(0, lastSeparator) : '';
	const combined = normalizedTarget.startsWith('/') || /^[A-Za-z]:\//.test(normalizedTarget)
		? normalizedTarget
		: `${fileDirectory}/${normalizedTarget}`;
	const drive = combined.match(/^([A-Za-z]:)\//);
	const unc = combined.startsWith('//');
	const absolute = !unc && combined.startsWith('/');
	let prefix = '';
	let remainder = combined;
	if (drive) {
		prefix = `${drive[1]}/`;
		remainder = combined.slice(drive[0].length);
	} else if (unc) {
		prefix = '//';
		remainder = combined.slice(2);
	} else if (absolute) {
		prefix = '/';
		remainder = combined.slice(1);
	}
	const resolved: string[] = [];

	for (const segment of remainder.split('/')) {
		if (!segment || segment === '.') continue;
		if (segment === '..') {
			resolved.pop();
		} else {
			resolved.push(segment);
		}
	}

	return prefix + resolved.join('/');
}

/** Lexical ownership for the UI; native code enforces filesystem access. */
export function isPathInDirectory(path: string, directory: string | null): boolean {
	if (!directory) return false;
	let root = resolvePathFromFile('', directory).replace(/\/+$/, '');
	let target = resolvePathFromFile('', path);
	if (/^[A-Za-z]:\//.test(root) || root.startsWith('//')) {
		root = root.toLowerCase();
		target = target.toLowerCase();
	}
	return target.startsWith(`${root}/`);
}

export function isTrashedNote(path: string | null, vaultRoot: string | null | undefined): boolean {
	return !!path && !!vaultRoot && isPathInDirectory(path, `${vaultRoot}/.ryuknotes/trash`);
}
