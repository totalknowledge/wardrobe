import { Component } from '@angular/core';
import { APP_VERSION } from '../app-version';
import { WardrobeService } from '../wardrobe/wardrobe-service';

@Component({
  selector: 'app-footer',
  imports: [],
  templateUrl: './footer.html',
  styleUrl: './footer.scss',
})
export class FooterComponent {
  public readonly appVersion = APP_VERSION;

  constructor(public databaseService: WardrobeService) {}
}
