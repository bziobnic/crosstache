const collator = new Intl.Collator(undefined, {
  sensitivity: 'base',
  numeric: true,
});

function normalizeSearchText(value) {
  return typeof value === 'string'
    ? value.normalize('NFKC').toLocaleLowerCase().trim()
    : '';
}

function normalizedList(value) {
  const values = Array.isArray(value)
    ? value
    : (typeof value === 'string' ? value.split(',') : []);
  return values.map(normalizeSearchText).filter(Boolean);
}

function fileFolder(file) {
  if (typeof file?.folder === 'string') return file.folder;
  const name = typeof file?.name === 'string' ? file.name : '';
  const separator = name.lastIndexOf('/');
  return separator < 0 ? '' : name.slice(0, separator);
}

function secretType(secret) {
  return secret?.tags?.['xv-type']
    || secret?.record_type
    || secret?.type
    || (secret?.content_type === 'application/vnd.xv.record' ? 'record' : 'plain');
}

function entry(surface, sourceIndex, name, folder, terms, searchName = name) {
  return Object.freeze({
    surface,
    sourceIndex,
    name: typeof name === 'string' ? name : '',
    folder: typeof folder === 'string' ? folder : '',
    normalizedName: normalizeSearchText(searchName),
    normalizedFolder: normalizeSearchText(folder),
    normalizedTerms: Object.freeze(terms.flatMap(normalizedList)),
  });
}

export function buildMetadataIndex({ secrets = [], files = [], folders = [] } = {}) {
  const entries = [
    ...secrets.map((secret, sourceIndex) => entry(
      'secrets',
      sourceIndex,
      secret?.original_name || secret?.name,
      secret?.folder,
      [secret?.groups, secretType(secret)],
    )),
    ...files.map((file, sourceIndex) => entry(
      'files',
      sourceIndex,
      file?.name,
      fileFolder(file),
      [file?.content_type],
      file?.name?.split('/').at(-1),
    )),
    ...folders.map((folder, sourceIndex) => entry(
      'folders',
      sourceIndex,
      folder,
      folder,
      [],
    )),
  ];
  return Object.freeze({ entries: Object.freeze(entries) });
}

function matchRank(entryValue, query) {
  if (entryValue === query) return 0;
  if (entryValue.startsWith(query)) return 1;
  if (entryValue.split(/[^\p{Letter}\p{Number}]+/u).some((word) => word.startsWith(query))) {
    return 2;
  }
  return entryValue.includes(query) ? 3 : null;
}

export function searchIndex(index, query) {
  const normalizedQuery = normalizeSearchText(query);
  if (!normalizedQuery) return [];
  const ranked = [];
  for (const indexed of index?.entries || []) {
    let score = matchRank(indexed.normalizedName, normalizedQuery);
    if (score === null && indexed.normalizedTerms.some((term) => term.includes(normalizedQuery))) {
      score = 4;
    }
    if (score === null && indexed.normalizedFolder.includes(normalizedQuery)) score = 5;
    if (score !== null) ranked.push({ indexed, score });
  }
  ranked.sort((left, right) => (
    left.score - right.score
      || collator.compare(left.indexed.name, right.indexed.name)
      || collator.compare(left.indexed.surface, right.indexed.surface)
      || left.indexed.sourceIndex - right.indexed.sourceIndex
  ));
  return ranked.map(({ indexed }) => indexed);
}

export { normalizeSearchText };
