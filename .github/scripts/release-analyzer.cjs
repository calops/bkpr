exports.analyzeCommits = async ({ forcePatch, ...options }, context) => {
  const { analyzeCommits } = await import('@semantic-release/commit-analyzer');
  const release = await analyzeCommits(options, context);
  return release || (forcePatch && context.commits.length > 0 ? 'patch' : null);
};
