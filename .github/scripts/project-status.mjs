import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const project = 'PVT_kwHOAD6GtM4Bsn8r';
const repository = 'calops/bkpr';
const page = 'pageInfo { hasNextPage endCursor }';
const prFields = 'id state isDraft';
const links = `closedByPullRequestsReferences(first: 20, includeClosedPrs: true) {
  ${page} nodes { ${prFields} }
}`;

function graphql(query, variables) {
  const result = JSON.parse(execFileSync('gh', ['api', 'graphql', '--input', '-'], {
    input: JSON.stringify({ query, variables }), encoding: 'utf8',
  }));
  if (result.errors?.length) throw new Error(JSON.stringify(result.errors));
  return result.data;
}

function* pages(read, first = read(null)) {
  let current = first;
  while (true) {
    yield* current.nodes;
    if (!current.pageInfo.hasNextPage) return;
    current = read(current.pageInfo.endCursor);
  }
}

function trackedIssues() {
  const byIssue = new Map();
  for (const pr of pages(after => graphql(`query($after: String) {
    repository(owner: "calops", name: "bkpr") {
      pullRequests(first: 100, after: $after, states: [OPEN, CLOSED, MERGED]) {
        ${page} nodes { ${prFields} body authorAssociation }
      }
    }
  }`, { after }).repository.pullRequests)) {
    if (!['OWNER', 'MEMBER', 'COLLABORATOR'].includes(pr.authorAssociation)) continue;
    for (const [, number] of pr.body.matchAll(/^Tracks[ \t]+(?:calops\/bkpr)?#([1-9]\d*)[ \t]*\r?$/gim)) {
      if (!byIssue.has(number)) byIssue.set(number, []);
      byIssue.get(number).push(pr);
    }
  }
  return byIssue;
}

export function statusFor(prs) {
  const open = prs.filter(pr => pr.state === 'OPEN');
  if (open.some(pr => !pr.isDraft)) return 'In review';
  if (open.length) return 'In progress';
  if (prs.length && prs.every(pr => pr.state === 'MERGED')) return 'Done';
  return null;
}

function reconcile(dryRun) {
  const board = graphql(`query($project: ID!) {
    node(id: $project) { ... on ProjectV2 {
      field(name: "Status") { ... on ProjectV2SingleSelectField { id options { id name } } }
    } }
  }`, { project }).node;
  if (!board?.field?.options) throw new Error('Cannot read the project Status field');
  const { id: field, options } = board.field;
  const optionIds = new Map(options.map(({ id, name }) => [name, id]));
  for (const name of ['In progress', 'In review', 'Done']) {
    if (!optionIds.has(name)) throw new Error(`Missing project status: ${name}`);
  }
  const tracked = trackedIssues();
  for (const { id: item, isArchived, fieldValue, content } of pages(after => graphql(`query($project: ID!, $after: String) {
    node(id: $project) { ... on ProjectV2 {
      items(first: 50, after: $after) { ${page} nodes {
        id isArchived
        fieldValue: fieldValueByName(name: "Status") { ... on ProjectV2ItemFieldSingleSelectValue { optionId } }
        content { ... on Issue { id number state repository { nameWithOwner } ${links} } }
      } }
    } }
  }`, { project, after }).node.items)) {
    if (isArchived || content?.state !== 'OPEN' || content.repository.nameWithOwner !== repository) continue;
    const prs = new Map((tracked.get(String(content.number)) ?? []).map(pr => [pr.id, pr]));
    for (const pr of pages(after => graphql(`query($issue: ID!, $after: String) {
      node(id: $issue) { ... on Issue {
        closedByPullRequestsReferences(first: 100, after: $after, includeClosedPrs: true) {
          ${page} nodes { ${prFields} }
        }
      } }
    }`, { issue: content.id, after }).node.closedByPullRequestsReferences, content.closedByPullRequestsReferences)) {
      prs.set(pr.id, pr);
    }
    const status = statusFor([...prs.values()]);
    if (!status) continue;
    const option = optionIds.get(status);
    if (fieldValue?.optionId === option) continue;
    if (!dryRun) graphql(`mutation($project: ID!, $item: ID!, $field: ID!, $option: String!) {
      updateProjectV2ItemFieldValue(input: {projectId: $project, itemId: $item, fieldId: $field,
        value: {singleSelectOptionId: $option}}) { projectV2Item { id } }
    }`, { project, item, field, option });
    console.log(`${dryRun ? 'Would set' : 'Set'} #${content.number} → ${status}`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  reconcile(process.argv.includes('--dry-run'));
}
