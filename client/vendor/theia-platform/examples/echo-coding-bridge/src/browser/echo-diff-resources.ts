import { injectable } from '@theia/core/shared/inversify';
import URI from '@theia/core/lib/common/uri';
import { InMemoryResources, Resource } from '@theia/core/lib/common/resource';

/** Diff baselines live for exactly the lifetime of their native model references. */
@injectable()
export class EchoDiffResources extends InMemoryResources {
    override resolve(uri: URI): Resource {
        if (uri.scheme !== 'echo-diff') { throw new Error('Not an Echo diff snapshot'); }
        const resource = super.resolve(uri);
        return { uri, readOnly: true, autosaveable: false,
            readContents: () => resource.readContents(), dispose: () => resource.dispose() };
    }
}
