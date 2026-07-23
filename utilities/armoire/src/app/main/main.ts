import { Component, OnInit } from '@angular/core';
import { Router } from '@angular/router';
import { WardrobeService } from '../wardrobe/wardrobe-service';
import { BayListviewComponent } from './bay-listview/bay-listview';
import { ConnectionTreeComponent } from './connection-tree/connection-tree';
import { DrawerListviewComponent } from './drawer-listview/drawer-listview';

@Component({
  selector: 'app-main',
  imports: [ConnectionTreeComponent, BayListviewComponent, DrawerListviewComponent],
  templateUrl: './main.html',
  styleUrl: './main.scss',
})
export class MainComponent implements OnInit {
  public selectedDatabase: string | null = null;
  public selectedBay: string | null = null;

  constructor(
    public databaseService: WardrobeService,
    private router: Router
  ) {}

  public ngOnInit(): void {
    // Load the app's saved connection metadata without opening a user database.
    this.databaseService.loadSavedConnections();
  }

  public get hasActiveConnection(): boolean {
    return this.databaseService.databaseStatus() === 'connected'
      && this.databaseService.activeConnectionPath() !== null;
  }

  public get hasBays(): boolean {
    return this.hasActiveConnection && this.databaseService.bays().length > 0;
  }

  public clearSelection(): void {
    this.selectedDatabase = null;
    this.selectedBay = null;
    this.databaseService.setSelectedContext(null, null);
  }

  public selectDatabase(databaseName: string): void {
    this.selectedDatabase = databaseName;
    this.selectedBay = null;
    this.databaseService.setSelectedContext(databaseName, null);

    this.databaseService.showBays(databaseName);
    this.databaseService.showDrawers(databaseName, 'default');
  }

  public selectBay(bayName: string): void {
    if (!this.selectedDatabase) return;
    this.selectedBay = bayName;
    this.databaseService.setSelectedContext(this.selectedDatabase, bayName);
    this.databaseService.showDrawers(this.selectedDatabase, bayName);
  }

  public viewDrawer(drawerName: string): void {
    this.router.navigate(['/drawer', drawerName]);
  }

}
