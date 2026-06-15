import { Service, signal } from '@angular/core';
import { GlobalWithTauri } from './wardrobe-tauri-types';

export type WardrobeCommandStatus = 'idle' | 'running' | 'success' | 'error';

@Service()
export class WardrobeService {
    public readonly testDatabaseAccessStatus = signal<WardrobeCommandStatus>('idle');
    public readonly testDatabaseAccessError = signal<string | null>(null);

    public testDatabaseAccess(databaseDirectory: string): void {
        this.testDatabaseAccessStatus.set('running');
        this.testDatabaseAccessError.set(null);

        void this.invokeWardrobeCommand<void>('wardrobe_test_database_access', {
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

    private invokeWardrobeCommand<T>(
        command: string,
        args?: Record<string, unknown>,
    ): Promise<T> {
        const tauri = (globalThis as GlobalWithTauri).__TAURI__;
        const invoke = tauri?.core?.invoke ?? tauri?.tauri?.invoke;

        if (!invoke) {
            return Promise.reject(
                new Error('Tauri invoke API is not available in this runtime.'),
            );
        }

        return invoke<T>(command, args);
    }
}