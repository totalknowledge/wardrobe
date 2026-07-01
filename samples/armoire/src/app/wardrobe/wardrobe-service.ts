import { Service, signal } from '@angular/core';
import { invoke } from './wardrobe-tauri';
import { WardrobeCommandStatus } from './wardrobe-tauri-definitions';

@Service()
export class WardrobeService {
    public readonly databaseStatus = signal<WardrobeCommandStatus>('disconnected');
    public readonly testDatabaseAccessError = signal<string | null>(null);

    public readonly databases = signal<any[]>([]);
    public readonly bays = signal<string[]>([]);
    public readonly drawers = signal<any[]>([]);
    public readonly selectedDatabaseName = signal<string | null>(localStorage.getItem('selected_database'));
    public readonly selectedSchemaName = signal<string | null>(localStorage.getItem('selected_schema'));
    public readonly currentDrawerRecords = signal<any[]>([]);

    public readonly isLoadingDatabases = signal<boolean>(false);
    public readonly isLoadingBays = signal<boolean>(false);
    public readonly isLoadingDrawers = signal<boolean>(false);
    public readonly isLoadingRecords = signal<boolean>(false);
    public readonly savedConnections = signal<any[]>([]);
    public readonly activeConnectionPath = signal<string | null>(null);

    private readonly tauri = invoke;

    public createSourceLocation(databaseDirectory: string): Promise<void> {
        return this.wardrobeCommand<void>('wardrobe_create_source_location', {
            databaseDirectory,
        });
    }

    public connectSourceLocation(databaseDirectory: string, name?: string): Promise<any[]> {
        this.databaseStatus.set('connecting');
        this.testDatabaseAccessError.set(null);
        this.setBusyCursor(true);

        return this.wardrobeCommand<void>('wardrobe_connect_source_location', {
            databaseDirectory,
            name,
        })
            .then(() => {
                this.databaseStatus.set('connected');
                this.activeConnectionPath.set(databaseDirectory);
                console.log('Connected to source location.');
                return this.showWardrobes();
            })
            .catch((error: unknown) => {
                const message = error instanceof Error ? error.message : String(error);

                this.databaseStatus.set('error');
                this.testDatabaseAccessError.set(message);

                console.error('Wardrobe database connection failed.', error);
                throw error;
            })
            .finally(() => {
                this.setBusyCursor(false);
            });
    }

    public disconnect(): void {
        this.databaseStatus.set('disconnected');
        this.databases.set([]);
        this.bays.set([]);
        this.drawers.set([]);
        this.activeConnectionPath.set(null);
        this.setSelectedContext(null, null);
        this.setBusyCursor(false);
    }

    public showWardrobes(): Promise<any[]> {
        this.isLoadingDatabases.set(true);
        return this.wardrobeCommand<any[]>('wardrobe_show_wardrobes')
            .then((databases) => {
                this.databases.set(databases);
                this.isLoadingDatabases.set(false);
                return databases;
            })
            .catch((error) => {
                console.error('Error showing wardrobes:', error);
                this.databases.set([]);
                this.isLoadingDatabases.set(false);
                return [];
            });
    }

    public showBays(databaseName: string): void {
        this.isLoadingBays.set(true);
        this.bays.set([]);
        void this.wardrobeCommand<string[]>('wardrobe_show_bays', {
            databaseName,
        })
            .then((bays) => {
                this.bays.set(bays);
                this.isLoadingBays.set(false);
            })
            .catch((error) => {
                console.error('Error showing bays:', error);
                this.bays.set([]);
                this.isLoadingBays.set(false);
            });
    }

    public showDrawers(databaseName: string, schemaName: string): void {
        this.isLoadingDrawers.set(true);
        this.drawers.set([]);
        void this.wardrobeCommand<any[]>('wardrobe_show_drawers', {
            databaseName,
            schemaName,
        })
            .then((drawers) => {
                if (schemaName === 'default' && drawers.length === 0) {
                    return this.wardrobeCommand<any[]>('wardrobe_show_drawers', {
                        databaseName,
                        schemaName: 'public',
                    });
                }
                return drawers;
            })
            .then((drawers) => {
                this.drawers.set(drawers);
                this.isLoadingDrawers.set(false);
            })
            .catch((error) => {
                console.error('Error showing drawers:', error);
                this.drawers.set([]);
                this.isLoadingDrawers.set(false);
            });
    }

    public createNewBay(databaseName: string, schemaName: string): void {
        void this.wardrobeCommand<void>('wardrobe_create_new_bay', {
            databaseName,
            schemaName,
        })
            .then(() => {
                this.showBays(databaseName);
            })
            .catch((error) => {
                console.error('Error creating bay:', error);
            });
    }

    public createNewDrawer(databaseName: string, schemaName: string, drawerName: string): void {
        void this.wardrobeCommand<void>('wardrobe_create_new_drawer', {
            databaseName,
            schemaName,
            drawerName,
        })
            .then(() => {
                this.showDrawers(databaseName, schemaName);
            })
            .catch((error) => {
                console.error('Error creating drawer:', error);
            });
    }

    public setSelectedContext(databaseName: string | null, schemaName: string | null): void {
        this.selectedDatabaseName.set(databaseName);
        this.selectedSchemaName.set(schemaName);
        if (databaseName) {
            localStorage.setItem('selected_database', databaseName);
        } else {
            localStorage.removeItem('selected_database');
        }
        if (schemaName) {
            localStorage.setItem('selected_schema', schemaName);
        } else {
            localStorage.removeItem('selected_schema');
        }
    }

    public readRecords(databaseName: string, schemaName: string, drawerName: string): void {
        this.isLoadingRecords.set(true);
        this.currentDrawerRecords.set([]);
        void this.wardrobeCommand<any[]>('wardrobe_read_records', {
            databaseName,
            schemaName,
            drawerName,
        })
            .then((records) => {
                this.currentDrawerRecords.set(records);
                this.isLoadingRecords.set(false);
            })
            .catch((error) => {
                console.error('Error reading records:', error);
                this.currentDrawerRecords.set([]);
                this.isLoadingRecords.set(false);
            });
    }

    public createRecord(
        databaseName: string,
        schemaName: string,
        drawerName: string,
        payload: any,
    ): Promise<void> {
        return this.wardrobeCommand<void>('wardrobe_create_record', {
            databaseName,
            schemaName,
            drawerName,
            payload,
        }).then(() => {
            this.readRecords(databaseName, schemaName, drawerName);
        });
    }

    public loadSavedConnections(): Promise<void> {
        return this.wardrobeCommand<any[]>('armoire_get_saved_connections')
            .then((connections) => {
                this.savedConnections.set(connections);
            })
            .catch((error) => {
                console.error('Error loading saved connections:', error);
            });
    }

    public removeSavedConnection(target: string): Promise<void> {
        return this.wardrobeCommand<void>('armoire_remove_connection', { target })
            .then(() => {
                return this.loadSavedConnections();
            })
            .catch((err) => {
                console.error('Error removing connection:', err);
            });
    }

    public testConnection(databaseDirectory: string): Promise<void> {
        return this.wardrobeCommand<void>('wardrobe_test_database_access', {
            databaseDirectory,
        });
    }

    public createNewWardrobe(databaseName: string): Promise<void> {
        return this.wardrobeCommand<void>('wardrobe_create_new_wardrobe', {
            databaseName,
        })
            .then(() => {
                this.showWardrobes();
            })
            .catch((error) => {
                console.error('Error creating wardrobe:', error);
            });
    }

    public updateConnectionAlias(target: string, alias: string): Promise<void> {
        return this.wardrobeCommand<void>('armoire_update_connection_alias', { target, alias })
            .then(() => {
                return this.loadSavedConnections();
            })
            .catch((err) => {
                console.error('Error updating alias:', err);
            });
    }

    public deleteConnectionFiles(target: string, id: string): Promise<void> {
        return this.wardrobeCommand<void>('armoire_delete_connection_files', { target, id })
            .then(() => {
                // If it was the active connection, reset active status
                if (this.activeConnectionPath() === target) {
                    this.databaseStatus.set('disconnected');
                    this.activeConnectionPath.set(null);
                }
                return this.loadSavedConnections();
            })
            .catch((err) => {
                console.error('Error deleting connection files:', err);
            });
    }

    private wardrobeCommand<T>(
        command: string,
        args?: Record<string, unknown>,
    ): Promise<T> {
        return this.tauri<T>(command, args);
    }

    private setBusyCursor(isBusy: boolean): void {
        document.documentElement.classList.toggle('app-busy', isBusy);
        document.body.classList.toggle('app-busy', isBusy);
    }
}
