'use strict';

const { PACKAGE_VERSION, WardrobeClient } = require('@wardrobe/embedded');

const databaseDirectory = './wardrobe';
const databaseName = 'publishing-house';
const bayName = 'public_js';
const publisherDrawer = 'publisher';
const personDrawer = 'person';
const bookDrawer = 'book';

function printSeparator(title) {
  console.log('\n==================================================');
  console.log(`>>> ${title}`);
  console.log('==================================================\n');
}

function firstPointer(result) {
  const pointers = result && (result.Pointers || result.pointers);
  if (!Array.isArray(pointers) || typeof pointers[0] !== 'string') {
    throw new Error('Upsert returned no pointer');
  }
  return pointers[0];
}

function records(result) {
  if (!Array.isArray(result)) {
    throw new Error('Expected record list');
  }
  return result;
}

async function main() {
  const metadataClient = await WardrobeClient.open(databaseDirectory);
  await metadataClient.create({ Database: { database_name: databaseName } });
  await metadataClient.create({
    Schema: { database_name: databaseName, schema_name: bayName }
  });
  for (const drawerName of [publisherDrawer, personDrawer, bookDrawer]) {
    await metadataClient.create({
      Drawer: {
        database_name: databaseName,
        schema_name: bayName,
        drawer_name: drawerName
      }
    });
  }

  const wardrobe = await WardrobeClient.open(
    `${databaseDirectory}/${databaseName}/${bayName}`
  );

  printSeparator('Phase 1: Metadata & Inventory Discovery');
  const databases = await metadataClient.status('Databases');
  const bays = await metadataClient.status({ Schemas: { database_name: databaseName } });
  const drawers = await metadataClient.status({
    Drawers: { database_name: databaseName, schema_name: bayName }
  });
  console.log('System Databases:', databases.map((database) => database.name));
  console.log(`Available Bays in '${databaseName}':`, bays);
  console.log(`Drawers in ${databaseName}/${bayName}:`);
  for (const drawer of drawers) {
    console.log(` - Drawer: ${drawer.name} (${drawer.record_count} records)`);
  }

  printSeparator('Phase 2: Relational Data Population');
  const publisherPointer = firstPointer(
    await wardrobe.upsert(
      {
        _id: 'pub_001',
        name: 'Apex Press',
        founded_year: 1994,
        active: true
      },
      publisherDrawer
    )
  );
  console.log(`Persisted publisher -> ${publisherPointer}`);

  const authorPointer = firstPointer(
    await wardrobe.upsert(
      {
        _id: 'author_001',
        name: 'Elena Vance',
        role: 'author',
        genres: ['sci-fi', 'thriller']
      },
      personDrawer
    )
  );
  console.log(`Persisted author (in person drawer) -> ${authorPointer}`);

  const editorPointer = firstPointer(
    await wardrobe.upsert(
      {
        _id: 'editor_001',
        name: 'Marcus Sterling',
        role: 'editor',
        department: 'fiction'
      },
      personDrawer
    )
  );
  console.log(`Persisted editor (in person drawer) -> ${editorPointer}`);

  const bookPointer = firstPointer(
    await wardrobe.upsert(
      {
        _id: 'book_001',
        title: 'The Quantum Horizon',
        publisher_id: publisherPointer,
        author_id: authorPointer,
        editor_id: editorPointer,
        page_count: 420
      },
      bookDrawer
    )
  );
  console.log(`Persisted book -> ${bookPointer}`);

  printSeparator('Phase 3: Filter Query Execution');
  const matchingPersonnel = records(
    await wardrobe.read([personDrawer, { role: 'author' }], {
      orderBy: 'name',
      orderDirection: 'asc',
      offset: 0,
      limit: 10
    })
  );
  console.log(`Found ${matchingPersonnel.length} matching personnel records:`);
  for (const person of matchingPersonnel) {
    console.log(`  - Match Found: ${JSON.stringify(person)}`);
  }

  printSeparator('Phase 4: Relation Verification');
  const verifiedBook = await wardrobe.read(bookPointer);
  const verifiedAuthor = await wardrobe.read(authorPointer);
  const verifiedEditor = await wardrobe.read(editorPointer);
  console.log(`Book lookup check: ${verifiedBook !== null}`);
  console.log(`Author lookup check: ${verifiedAuthor !== null}`);
  console.log(`Editor lookup check: ${verifiedEditor !== null}`);

  printSeparator('Phase 5: Maintenance & Stress Test Cycle');
  const personCount = await wardrobe.count(personDrawer);
  const bookCount = await wardrobe.count(bookDrawer);
  console.log(`Maintenance check: ${personCount} personnel, ${bookCount} books active`);
  for (let index = 0; index < 5; index += 1) {
    const pointer = firstPointer(
      await wardrobe.upsert(
        {
          _id: `temp_book_${index}`,
          title: 'Temporary Draft',
          page_count: 100
        },
        bookDrawer
      )
    );
    await wardrobe.delete(pointer);
  }
  console.log('Stress test cycle completed (5 temporary book upserts/deletes).');

  printSeparator('Phase 6: Detailed Embedded Inspection');
  for (const drawer of drawers) {
    const count = await wardrobe.count(drawer.name);
    console.log(`Drawer: ${drawer.name} (${count} records)`);
  }

  printSeparator('Phase 7: Final State Reconciliation & Integrity');
  const remainingPersonnel = records(await wardrobe.read(personDrawer));
  const remainingBooks = records(await wardrobe.read(bookDrawer));
  const publisher = await wardrobe.read(publisherPointer);
  console.log(
    `Total active records: ${remainingPersonnel.length} personnel (authors/editors), ` +
      `${remainingBooks.length} books`
  );
  console.log(
    publisher === null
      ? 'INTEGRITY NOTE: Publisher record was not found.'
      : `INTEGRITY SUCCESS: Publisher record persists intact: ${JSON.stringify(publisher)}`
  );

  console.log(`\nWardrobe JavaScript binding ${PACKAGE_VERSION}`);
  console.log('Publishing domain integration sample completed successfully. All 7 phases completed.');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
