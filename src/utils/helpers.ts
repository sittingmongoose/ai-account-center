import * as path from 'path';
import * as os from 'os';

/**
 * Simple error formatting
 */
export function error(message: string): never {
  console.error(`ERROR: ${message}`);
  console.error(
    'Setup and recovery: https://github.com/sittingmongoose/ai-account-center/tree/feat/activate-in-place#install-from-current-source'
  );
  process.exit(1);
}

/**
 * Path expansion (~ and env vars)
 */
export function expandPath(pathStr: string): string {
  // Normalize separators first to handle mixed paths
  pathStr = pathStr.replace(/\\/g, '/');

  // Handle tilde expansion
  if (pathStr.startsWith('~/')) {
    pathStr = path.join(os.homedir(), pathStr.slice(2));
  } else if (pathStr === '~') {
    pathStr = os.homedir();
  }

  // Expand environment variables (Windows and Unix)
  pathStr = pathStr.replace(/\$\{([^}]+)\}/g, (_, name) => process.env[name] || '');
  pathStr = pathStr.replace(/\$([A-Z_][A-Z0-9_]*)/gi, (_, name) => process.env[name] || '');

  // Windows %VAR% style
  if (process.platform === 'win32') {
    pathStr = pathStr.replace(/%([^%]+)%/g, (_, name) => process.env[name] || '');
  }

  return path.normalize(pathStr);
}

/**
 * Calculate Levenshtein distance between two strings
 */
export function levenshteinDistance(a: string, b: string): number {
  if (a.length === 0) return b.length;
  if (b.length === 0) return a.length;

  const matrix: number[][] = [];

  // Initialize first row and column
  for (let i = 0; i <= b.length; i++) {
    matrix[i] = [i];
  }

  for (let j = 0; j <= a.length; j++) {
    matrix[0][j] = j;
  }

  // Fill in the rest of the matrix
  for (let i = 1; i <= b.length; i++) {
    for (let j = 1; j <= a.length; j++) {
      if (b.charAt(i - 1) === a.charAt(j - 1)) {
        matrix[i][j] = matrix[i - 1][j - 1];
      } else {
        matrix[i][j] = Math.min(
          matrix[i - 1][j - 1] + 1, // substitution
          matrix[i][j - 1] + 1, // insertion
          matrix[i - 1][j] + 1 // deletion
        );
      }
    }
  }

  return matrix[b.length][a.length];
}

/**
 * Find similar strings using fuzzy matching
 */
export function findSimilarStrings(
  target: string,
  candidates: string[],
  maxDistance: number = 2
): string[] {
  const targetLower = target.toLowerCase();

  const matches = candidates
    .map((candidate) => ({
      name: candidate,
      distance: levenshteinDistance(targetLower, candidate.toLowerCase()),
    }))
    .filter((item) => item.distance <= maxDistance && item.distance > 0)
    .sort((a, b) => a.distance - b.distance)
    .slice(0, 3) // Show at most 3 suggestions
    .map((item) => item.name);

  return matches;
}
