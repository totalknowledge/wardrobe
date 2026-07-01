import { Component, EventEmitter, Input, Output } from '@angular/core';
import { DialogService } from '../../dialog/dialog-service';
import { WardrobeService } from '../../wardrobe/wardrobe-service';

@Component({
  selector: 'app-connection-tree',
  imports: [],
  templateUrl: './connection-tree.html',
  styleUrl: './connection-tree.scss',
})
export class ConnectionTreeComponent {
  @Input() public selectedDatabase: string | null = null;

  @Output() public readonly databaseSelected = new EventEmitter<string>();
  @Output() public readonly connectionCleared = new EventEmitter<void>();

  constructor(
    public databaseService: WardrobeService,
    private dialogService: DialogService,
  ) {}

  public createWardrobe(): void {
    const dbName = prompt('Enter the name for the new wardrobe (database):');
    if (dbName && dbName.trim()) {
      this.databaseService.createNewWardrobe(dbName.trim());
    }
  }

  public selectSavedConnection(target: string): void {
    if (this.databaseService.activeConnectionPath() === target) {
      this.databaseService.disconnect();
      this.connectionCleared.emit();
      return;
    }

    this.connectionCleared.emit();
    void this.databaseService.connectSourceLocation(target).then((databases) => {
      if (databases.length === 1) {
        this.databaseSelected.emit(databases[0].name);
      }
    });
  }

  public selectDatabase(databaseName: string): void {
    this.databaseSelected.emit(databaseName);
  }

  public isFlatfile(target: string): boolean {
    return !target.startsWith('wardrobe://') && !target.includes('://');
  }

  public openConnectionMenu(event: Event, conn: any): void {
    event.stopPropagation();
    const target = conn.target || conn._id;
    const isLocal = this.isFlatfile(target);

    const actionsList = [
      {
        label: 'Edit (Rename)',
        class: 'btn btn-sm btn-info',
        onClick: () => {
          setTimeout(() => {
            this.editConnection(target, conn.name || conn.alias || '');
          }, 150);
        },
      },
      {
        label: 'Remove from list',
        class: 'btn btn-sm btn-warning',
        onClick: () => {
          this.databaseService.removeSavedConnection(conn._id);
          if (this.databaseService.activeConnectionPath() === target) {
            this.databaseService.disconnect();
            this.connectionCleared.emit();
          }
        },
      },
    ];

    if (isLocal) {
      actionsList.push({
        label: 'Delete files & database',
        class: 'btn btn-sm btn-error',
        onClick: () => {
          setTimeout(() => {
            this.deleteConnection(target, conn._id);
          }, 150);
        },
      });
    }

    this.dialogService.openDialog({
      title: 'Connection Actions',
      body: `Manage saved connection: "${conn.name || conn.alias || target}"`,
      actions: actionsList,
    });
  }

  private editConnection(target: string, currentAlias: string): void {
    this.dialogService.openDialog({
      title: 'Rename Connection',
      body: `Enter a friendly name/alias for connection "${target}":`,
      showInput: true,
      inputValue: currentAlias,
      confirmText: 'Save',
      cancelText: 'Cancel',
      onConfirm: (newAlias) => {
        this.databaseService.updateConnectionAlias(target, newAlias || '');
      },
    });
  }

  private deleteConnection(target: string, id: string): void {
    if (confirm(`WARNING: Are you sure you want to permanently DELETE the database folder at "${target}" and all its contents from your disk? This action cannot be undone!`)) {
      this.databaseService.deleteConnectionFiles(target, id);
    }
  }
}
