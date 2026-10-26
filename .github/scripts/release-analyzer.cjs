exports.analyzeCommits = async ({ forcePatch, ...options }, context) => {
  const { analyzeCommits } = await import('@semantic-release/commit-analyzer');
  const release = await analyzeCommits(options, context);
  const bump = release === 'major' && context.lastRelease?.version?.startsWith('0.')
    ? 'minor'
    : release;
  return bump || (forcePatch && context.commits.length > 0 ? 'patch' : null);
};
