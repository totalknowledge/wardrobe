import { Component, EventEmitter, Input, Output } from '@angular/core';
import { WardrobeService } from '../../wardrobe/wardrobe-service';

@Component({
  selector: 'app-bay-listview',
  imports: [],
  templateUrl: './bay-listview.html',
  styleUrl: './bay-listview.scss',
})
export class BayListviewComponent {
  @Input() public selectedDatabase: string | null = null;
  @Input() public selectedBay: string | null = null;

  @Output() public readonly baySelected = new EventEmitter<string>();

  constructor(public databaseService: WardrobeService) {}

  public selectBay(bayName: string): void {
    this.baySelected.emit(bayName);
  }

  public createBay(): void {
    if (!this.selectedDatabase) return;
    const bayName = prompt('Enter the name for the new bay:');
    if (bayName && bayName.trim()) {
      this.databaseService.createNewBay(this.selectedDatabase, bayName.trim());
    }
  }
}
