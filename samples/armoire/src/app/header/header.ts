import { AfterViewInit, Component } from '@angular/core';
import { RouterLink } from '@angular/router';
import { APP_VERSION } from '../app-version';
import { DialogService } from '../dialog/dialog-service';
import { WardrobeService } from '../wardrobe/wardrobe-service';

@Component({
  selector: 'app-header',
  imports: [RouterLink],
  templateUrl: './header.html',
  styleUrl: './header.scss',
})
export class HeaderComponent implements AfterViewInit {

  constructor(
    private databaseService: WardrobeService,
    private dialogService: DialogService,
  ) {  }

  public ngAfterViewInit(): void { }

  public closeMenus(menu: HTMLDetailsElement): void {
    menu.removeAttribute('open');
  }

  public openConnectionModal(type: 'location' | 'connection'): void {
    this.dialogService.openConnectionModal(type);
  }

  public openCreateBayDialog(): void {

    const selectedDatabase = this.databaseService.selectedDatabaseName();
    if (!selectedDatabase) {
      this.dialogService.openDialog({
        title: 'Create Bay',
        body: 'Select a database first, then create a bay for it.',
      });
      return;
    }

    this.dialogService.openDialog({
      title: 'Create Bay',
      body: `Create a new bay inside "${selectedDatabase}".`,
      showInput: true,
      inputPlaceholder: 'Bay name',
      confirmText: 'Create',
      onConfirm: (bayName) => {
        const trimmedBayName = bayName?.trim();
        if (!trimmedBayName) {
          return;
        }
        this.databaseService.createNewBay(selectedDatabase, trimmedBayName);
      },
    });
  }

  public disconnect(): void {
    if (!this.canDisconnect()) {
      return;
    }
    this.databaseService.disconnect();
  }

  public canDisconnect(): boolean {
    return this.databaseService.databaseStatus() === 'connected'
      && this.databaseService.activeConnectionPath() !== null;
  }

  public about(): void {
    this.dialogService.openDialog({
      title: 'About Armoire',
      body: 'Armoire is a professional database management for use with the Wardrobe database engine. It provides a high-performance environment for schema architecture, data lifecycle control, and relationship management.',
      version: APP_VERSION,
    });
  }
}
