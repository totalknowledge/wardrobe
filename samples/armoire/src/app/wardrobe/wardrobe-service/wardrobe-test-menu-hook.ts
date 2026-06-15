import { invoke } from '@tauri-apps/api/core';

document.addEventListener('click', async (event) => {
  const target = event.target;
  if (!(target instanceof HTMLElement)) {
    return;
  }

  const trigger = target.closest('[data-wardrobe-test-database]');
  if (!trigger) {
    return;
  }

  event.preventDefault();
  await invoke<void>('wardrobe_test_database_access', {
    databaseDirectory: './wardrobe',
  });
  console.log('Wardrobe database access command completed.');
});
