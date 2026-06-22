import { Service, signal } from '@angular/core';
import { defer } from 'rxjs';
import { invoke } from './wardrobe-tauri';

export type WardrobeCommandStatus = 'idle' | 'running' | 'success' | 'error';

@Service()
export class WardrobeService {
    public readonly testDatabaseAccessStatus = signal<WardrobeCommandStatus>('idle');
    public readonly testDatabaseAccessError = signal<string | null>(null);
    private readonly tauri = invoke;

    public createSourceLocation(databaseDirectory: string): void {
        void this.wardrobeCommand('wardrobe_create_source_location', {
            databaseDirectory,
        });
    }

    public testDatabaseAccess(databaseDirectory: string): void {
        this.testDatabaseAccessStatus.set('running');
        this.testDatabaseAccessError.set(null);

        void this.wardrobeCommand<void>('wardrobe_test_database_access', {
            databaseDirectory
        })
            .then(() => {
                this.testDatabaseAccessStatus.set('success');
                console.log('Wardrobe database access command completed.');
            })
            .catch((error: unknown) => {
                const message = error instanceof Error ? error.message : String(error);

                this.testDatabaseAccessStatus.set('error');
                this.testDatabaseAccessError.set(message);

                console.error('Wardrobe database access command failed.', error);
            });
    }

    private wardrobeCommand<T>(
        command: string,
        args?: Record<string, unknown>,
    ): Promise<T> {
        return this.tauri<T>(command, args);
    }
}
