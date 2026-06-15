import { Injectable } from '@angular/core';
import { invoke } from '@tauri-apps/api/core';

@Injectable({ providedIn: 'root' })
export class WardrobeDatabaseService {
  public testDatabaseAccess(databaseDirectory: string): Promise<void> {
    return invoke<void>('wardrobe_test_database_access', {
      databaseDirectory,
    });
  }
}
