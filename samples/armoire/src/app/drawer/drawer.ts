import { Component, OnInit } from '@angular/core';
import { ActivatedRoute, RouterLink, Router } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { WardrobeService } from '../wardrobe/wardrobe-service';

@Component({
  selector: 'app-drawer',
  imports: [RouterLink, FormsModule],
  templateUrl: './drawer.html',
  styleUrl: './drawer.scss',
})
export class Drawer implements OnInit {
  public databaseName = '';
  public schemaName = '';
  public drawerName = '';

  // Modal State
  public showCreateModal = false;
  public newRecordJson = '';
  public jsonError: string | null = null;
  public isSaving = false;

  constructor(
    private route: ActivatedRoute,
    private router: Router,
    public databaseService: WardrobeService
  ) {}

  public goToWardrobe(): void {
    this.databaseService.setSelectedContext(this.databaseName, null);
    this.router.navigate(['/']);
  }

  public goToBay(): void {
    this.databaseService.setSelectedContext(this.databaseName, this.schemaName);
    this.router.navigate(['/']);
  }

  ngOnInit(): void {
    this.route.paramMap.subscribe(params => {
      this.drawerName = params.get('drawerName') || '';
      
      this.databaseName = this.databaseService.selectedDatabaseName() || '';
      this.schemaName = this.databaseService.selectedSchemaName() || 'default';
      
      if (this.databaseName && this.drawerName) {
        this.databaseService.readRecords(this.databaseName, this.schemaName, this.drawerName);
      }
    });
  }

  public openCreateModal(): void {
    const defaultTemplate = {
      name: ""
    };
    this.newRecordJson = JSON.stringify(defaultTemplate, null, 2);
    this.jsonError = null;
    this.showCreateModal = true;
  }

  public closeCreateModal(): void {
    this.showCreateModal = false;
    this.newRecordJson = '';
    this.jsonError = null;
  }

  public saveRecord(): void {
    this.jsonError = null;
    let parsedPayload: any;
    
    try {
      parsedPayload = JSON.parse(this.newRecordJson);
    } catch (e: any) {
      this.jsonError = `Invalid JSON format: ${e.message}`;
      return;
    }
    
    this.isSaving = true;
    this.databaseService.createRecord(this.databaseName, this.schemaName, this.drawerName, parsedPayload)
      .then(() => {
        this.isSaving = false;
        this.closeCreateModal();
      })
      .catch((error: unknown) => {
        this.isSaving = false;
        this.jsonError = error instanceof Error ? error.message : String(error);
      });
  }

  public getJsonString(value: any): string {
    return JSON.stringify(value, null, 2);
  }
}
