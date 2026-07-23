import { Component, EventEmitter, Input, Output } from '@angular/core';
import { WardrobeService } from '../../wardrobe/wardrobe-service';

@Component({
  selector: 'app-drawer-listview',
  imports: [],
  templateUrl: './drawer-listview.html',
  styleUrl: './drawer-listview.scss',
})
export class DrawerListviewComponent {
  @Input() public selectedDatabase: string | null = null;
  @Input() public selectedBay: string | null = null;

  @Output() public readonly drawerSelected = new EventEmitter<string>();

  constructor(public databaseService: WardrobeService) {}

  public viewDrawer(drawerName: string): void {
    this.drawerSelected.emit(drawerName);
  }

  public createDrawer(): void {
    if (!this.selectedDatabase) return;
    const schemaName = this.selectedBay || 'default';
    const drawerName = prompt(`Enter the name for the new drawer in bay "${schemaName}":`);
    if (drawerName && drawerName.trim()) {
      this.databaseService.createNewDrawer(this.selectedDatabase, schemaName, drawerName.trim());
    }
  }
}
